use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use rayon::prelude::*;

use crate::protocol::{
    Command, ConductorEvent, DependentFile, FileClassification, RenderMode, Response,
};
use crate::suggestion_apply::{self, DOCUMENT_KIND};
use content_store::edit;

mod editor_sync;
mod page_diagnostics;

/// Element kind given to a document root that has been superseded by a
/// re-ingest of the same file (ADR-056). See `Conductor::retire_document_root`.
const SUPERSEDED_DOCUMENT_KIND: &str = "superseded-document";

/// Stem → URL → document root. Keyed by URL so every reader sees a stem's
/// documents in URL order (ADR-058 D2), however they were added or edited.
type StemRoots = HashMap<String, BTreeMap<String, node_store::NodeId>>;

/// URL of the collection page for `stem` (`"/"` for the root stem).
///
/// Delegates to `site_index::url_for_stem_slug` so no URL is built by hand.
fn collection_url(stem: &str) -> String {
    site_index::url_for_stem_slug(stem, "index")
}

/// `stem_roots` in the `stem → roots` shape
/// `content_bridge::create_semantic_content` takes, each list in URL order.
fn stem_root_lists(stem_roots: &StemRoots) -> HashMap<String, Vec<node_store::NodeId>> {
    stem_roots
        .iter()
        .map(|(stem, roots)| (stem.clone(), roots.values().copied().collect()))
        .collect()
}

// ---------------------------------------------------------------------------
// Clojure string literal escaping
// ---------------------------------------------------------------------------

/// What running a NED program changed (see [`Conductor::execute_plan`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramOutcome {
    /// Files of the documents the program changed, marked dirty.
    pub dirty_paths: Vec<PathBuf>,
    /// URLs of the pages re-rendered.
    pub rebuilt_pages: Vec<String>,
    /// URLs of the pages that failed to re-render.
    pub failed_pages: Vec<String>,
    /// How many steps changed something.
    pub applied: usize,
    /// How many steps were already in place and dropped from the plan.
    pub skipped: usize,
}

/// Why a NED program did not run (see [`Conductor::execute_plan`]). Nothing
/// was applied in either case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// The program could not be evaluated.
    Eval(String),
    /// The program evaluated, but its plan was refused at one step.
    Plan(editorial_types::PlanFailure),
}

impl std::fmt::Display for ProgramError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProgramError::Eval(e) => write!(f, "eval error: {e}"),
            ProgramError::Plan(failure) => f.write_str(&failure.message),
        }
    }
}

impl std::error::Error for ProgramError {}

/// What applying one NED mutation did (see `Conductor::apply_ned_mutation`).
enum MutationOutcome {
    /// The document changed: its pages were re-rendered.
    Changed {
        pages: Vec<String>,
        failed: Vec<String>,
    },
    /// The content already reads as the mutation wants it: nothing was
    /// written, marked dirty or rebuilt.
    AlreadyInPlace,
}

/// The result of handling a command: a response to send back, plus
/// zero or more events to broadcast to all subscribers.
pub struct CommandResult {
    pub response: Response,
    pub events: Vec<ConductorEvent>,
}

/// The event that tells subscribers these documents were written to disk.
fn saved_event<'a>(files: impl IntoIterator<Item = &'a PathBuf>) -> ConductorEvent {
    let mut files: Vec<String> = files
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    files.sort();
    ConductorEvent::DocumentsSaved { files }
}

impl CommandResult {
    fn ok() -> Self {
        Self {
            response: Response::Ok,
            events: vec![],
        }
    }

    fn ok_with_events(events: Vec<ConductorEvent>) -> Self {
        Self {
            response: Response::Ok,
            events,
        }
    }

    fn error(msg: impl Into<String>) -> Self {
        Self {
            response: Response::Error(msg.into()),
            events: vec![],
        }
    }

    fn with_response(response: Response) -> Self {
        Self {
            response,
            events: vec![],
        }
    }
}

/// Convert a 0-based line number to a byte offset in `src`.
///
/// The offset points to the first byte of that line. If `line` exceeds the
/// number of lines in `src`, the offset of the last byte is returned.
fn line_to_byte_offset(src: &str, line: u32) -> usize {
    src.lines()
        .take(line as usize)
        .map(|l| l.len() + 1) // +1 for newline
        .sum()
}

/// Derive a [`StructuralAnchor`] for `body_idx` in `doc`.
///
/// Walks back from `body_idx` to find the nearest preceding heading, then
/// counts same-kind elements between that heading and `body_idx` to determine
/// the offset.  Returns `None` if `body_idx` is out of range or the element
/// kind cannot be mapped.
fn body_idx_to_structural(
    doc: &content::Document,
    file: &str,
    body_idx: usize,
) -> Option<editorial_types::StructuralAnchor> {
    if body_idx >= doc.body.len() {
        return None;
    }
    let target_kind = body_element_kind(&doc.body[body_idx].node)?;

    let mut heading_idx: Option<usize> = None;
    let mut heading_text: Option<String> = None;
    for i in (0..body_idx).rev() {
        if let content::ContentElement::Heading { content, .. } = &doc.body[i].node {
            heading_idx = Some(i);
            // The heading's plain text, as rendered and as
            // `ned/heading-with-text` matches it (ADR-063); already
            // whitespace-normalised.
            heading_text = Some(content::AsPlainText::plain_text(content));
            break;
        }
    }

    let offset = match heading_idx {
        Some(h) => doc
            .body
            .iter()
            .skip(h + 1)
            .take(body_idx - (h + 1))
            .filter(|s| body_element_kind(&s.node).as_deref() == Some(&target_kind))
            .count(),
        None => doc
            .body
            .iter()
            .take(body_idx)
            .filter(|s| body_element_kind(&s.node).as_deref() == Some(&target_kind))
            .count(),
    };

    Some(editorial_types::StructuralAnchor {
        file: file.to_string(),
        slot: "body".to_string(),
        heading_text,
        node_kind: target_kind,
        offset,
    })
}

/// Map a [`content::ContentElement`] to its structural kind string, or `None`
/// for elements that are not individually addressable (separators, links, etc.).
fn body_element_kind(el: &content::ContentElement) -> Option<String> {
    use content::ContentElement::*;
    Some(match el {
        Heading { .. } => "heading".to_string(),
        Paragraph { .. } => "paragraph".to_string(),
        CodeBlock { .. } => "code-block".to_string(),
        Table { .. } => "table".to_string(),
        Blockquote { .. } => "blockquote".to_string(),
        List { .. } => "list".to_string(),
        // An HTML block renders as a placeholder that is not a slot
        // (ADR-062 D2), so the browser has no element to anchor to.
        Separator | LinkExpression { .. } | HtmlBlock { .. } => return None,
    })
}

/// The names a template can be called by, as the template registry resolves
/// them: `templates/<name>/item.{hiccup,html}` and `templates/<name>.{hiccup,html}`.
/// Sorted and deduplicated.
fn template_names(site_dir: &Path) -> Vec<String> {
    let is_template = |p: &Path| {
        matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("hiccup" | "html")
        )
    };
    let mut names = std::collections::BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(site_dir.join("templates")) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if path.is_dir() {
            if ["item.hiccup", "item.html"]
                .iter()
                .any(|f| path.join(f).is_file())
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                names.insert(name.to_string());
            }
        } else if is_template(&path) {
            names.insert(stem);
        }
    }
    names.into_iter().collect()
}

#[allow(dead_code)]
pub struct Conductor {
    site_dir: PathBuf,
    output_dir: PathBuf,
    dep_graph: RwLock<dep_graph::DependencyGraph>,
    schema_cache: RwLock<HashMap<String, String>>, // stem -> schema source
    /// The grammar parsed from each schema source in `schema_cache`, by the same
    /// key, with the source it was parsed from (a changed source is parsed
    /// again): a page's diagnostics do not parse its schema once per page.
    parsed_grammars: RwLock<HashMap<String, (String, Arc<schema::Grammar>)>>,
    /// The documents an editor has open, each with the text N and the editor
    /// last agreed on (LSP-2).
    editors: RwLock<HashMap<editorial_types::ContentPath, editor_sync::EditorState>>,
    site_index: RwLock<site_index::SiteIndex>,
    repo: site_repository::SiteRepository,
    build_errors: RwLock<HashMap<String, Vec<String>>>,
    /// `(page url, message)` for unsaved edits the last repopulation could not
    /// keep; drained into `build_errors` by `flush_repopulate_failures`.
    repopulate_failures: RwLock<Vec<(String, String)>>,
    node_store: Arc<RwLock<node_store::NodeStore>>,
    url_to_root: RwLock<HashMap<String, node_store::NodeId>>,
    url_to_semantic: RwLock<HashMap<String, node_store::NodeId>>,
    stem_to_roots: RwLock<StemRoots>,
    // NodeStore-native cached indexes (Phase D)
    cached_node_url_index: RwLock<node_store_bridge::store_pipeline::NodeUrlIndex>,
    cached_node_stem_index: RwLock<node_store_bridge::store_pipeline::NodeStemIndex>,
    // NED edit path: tracks which content files have unsaved NodeStore mutations (Phase B)
    dirty_docs: RwLock<crate::dirty::DirtyDocs>,
    // What each content file looked like when we last read or wrote it, for the
    // save-time check that catches a change the watcher has not reported yet
    // (ADR-065). Not the self-write tracker: that one forgets after a second.
    disk_stamps: RwLock<crate::dirty::DiskStamps>,
    // The file each dirty document's unsaved edits are based on, for merging a
    // later disk change block by block (ADR-065 amendment F2).
    disk_bases: RwLock<crate::dirty::DiskBases>,
    /// Test hook: run once, between reading a file and taking its second
    /// stamp, to write the file there (the race the double stamp closes).
    #[cfg(test)]
    after_read_hook: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    // Map from content file path (relative to site_dir, matching `file` attribute on doc root)
    // to the NodeId of the document root. Populated alongside url_to_root.
    path_to_doc_root: RwLock<HashMap<PathBuf, node_store::NodeId>>,
    // NED-based suggestions (Phase C)
    ned_suggestions: RwLock<HashMap<editorial_types::SuggestionId, editorial_types::NedSuggestion>>,
    // Self-write tracker: suppresses watcher events for files the conductor wrote itself.
    self_write_tracker: crate::self_write_tracker::SelfWriteTracker,
    // Lookup index stem → stem node (`store_pipeline::create_stem_node`). Not
    // dependency data: the dependencies are the `Derived` edges in the store.
    // Reset whenever the store is repopulated.
    stem_nodes: RwLock<HashMap<String, node_store::NodeId>>,
    // Diagnostics for every schema and content file, rebuilt by
    // `populate_node_store`. The publish gate (ADR-061).
    validation_report: RwLock<validation::ValidationReport>,
}

/// A document whose content changed, as input to
/// [`Conductor::propagate_document_changes`].
pub(crate) struct DocChange {
    /// The page URL.
    pub url: String,
    /// The schema stem.
    pub stem: String,
    /// The live document root.
    pub doc_root: node_store::NodeId,
    /// The root this document replaced, when it was re-ingested. The retired
    /// root keeps its stored preamble fingerprint.
    pub previous_root: Option<node_store::NodeId>,
}

/// Extract the title from a document's preamble in the NodeStore.
/// Walks: document → preamble → slot(name="title") → first child element → first child text.
fn find_document_title(
    store: &node_store::NodeStore,
    doc_root: node_store::NodeId,
) -> Option<String> {
    let preamble =
        node_store_bridge::content_bridge::find_child_by_name(store, doc_root, "preamble")?;

    // Look through slots for one named "title"
    for child_id in store.children(preamble) {
        if let Some(node_store::Node::Element(name)) = store.get(child_id)
            && store.resolve_name(*name) == "slot"
            && let Some(slot_name) =
                node_store_bridge::content_bridge::find_attr_text(store, child_id, "name")
            && slot_name == "title"
        {
            for grandchild in store.children(child_id) {
                for text_child in store.children(grandchild) {
                    if let Some(node_store::Node::Text(s)) = store.get(text_child) {
                        return Some(s.clone());
                    }
                }
            }
        }
    }
    None
}

/// Result of rendering a single page: URL and outcome (success or error details).
type PageRenderResult = (String, Result<(), (String, Vec<String>)>);

/// One change to what the suggestion records mean (ADR-059): from some point
/// on, a kind of block means something else (holds inline nodes, or is no
/// longer a block kind), and a pending suggestion made before that point which
/// touches such a block no longer means what it said.
struct ModelStep {
    /// What `.presemble/suggestions/ned/MODEL` holds once the step has run.
    name: &'static str,
    /// The element kinds the step is about.
    kinds: &'static [&'static str],
    /// Completes "made before …" in the log line for the step.
    before: &'static str,
}

/// The steps in the order they were introduced.
const MODEL_STEPS: &[ModelStep] = &[
    ModelStep {
        name: "heading-inlines",
        kinds: &["heading"],
        before: "headings stored inline nodes",
    },
    ModelStep {
        name: "paragraph-inlines",
        kinds: &["paragraph"],
        before: "paragraphs stored inline nodes",
    },
    // A lone link or image became a paragraph (ADR-059 §12): a `link` or
    // `image` block in a payload would be refused on accept.
    ModelStep {
        name: "link-image-paragraphs",
        kinds: &["link", "image"],
        before: "a lone link or image was a paragraph",
    },
    // Table cells became inline containers (ADR-059 §6): a payload holding a
    // table in the old shape (`headers`, cells as text) would be refused on
    // accept.
    ModelStep {
        name: "table-cells",
        kinds: &["table"],
        before: "table cells stored inline nodes",
    },
    // Blockquotes became containers of blocks (ADR-059 §13): a payload holding
    // a blockquote in the old shape (one text child) would be refused on
    // accept, and one in the new shape was made against a model it never saw.
    ModelStep {
        name: "blockquote-containers",
        kinds: &["blockquote"],
        before: "blockquotes held blocks",
    },
    // Lists became containers of items of blocks (ADR-059 stage 9): a payload
    // holding a list in the old shape (one text child) would be refused on
    // accept, and one in the new shape was made against a model it never saw.
    ModelStep {
        name: "list-containers",
        kinds: &["list"],
        before: "lists held items of blocks",
    },
];

/// The steps still to run when `recorded` is the last one applied (nothing
/// recorded: all of them).
///
/// A recorded name this conductor does not know was written by a newer one:
/// nothing runs, and nothing is recorded over it.
fn model_steps_after(recorded: Option<&str>) -> &'static [ModelStep] {
    match recorded {
        None => MODEL_STEPS,
        Some(name) => match MODEL_STEPS.iter().position(|step| step.name == name) {
            Some(done) => &MODEL_STEPS[done + 1..],
            None => &[],
        },
    }
}

impl Conductor {
    pub fn new(site_dir: PathBuf) -> Result<Self, String> {
        let conductor = Self::load(site_dir)?;
        conductor.migrate_suggestion_records();
        // Build all pages during startup so the output directory is populated immediately.
        conductor.build_all_pages(template::OutputMode::Editorial);
        Ok(conductor)
    }

    /// Load and validate the site without rendering any pages: nothing is
    /// written to the output directory. The result of validation is in
    /// [`Conductor::validation_report`].
    pub fn load(site_dir: PathBuf) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&site_dir)
            .build();
        Self::load_with_repo(site_dir, repo)
    }

    /// The same conductor, writing its rendered pages to `output_dir` instead
    /// of the editorial output directory. Used by the published build, which
    /// renders the files on disk into a directory of its own.
    pub fn with_output_dir(mut self, output_dir: PathBuf) -> Self {
        self.output_dir = output_dir;
        self
    }

    /// Create a conductor with a pre-built repository. Used in tests.
    pub fn with_repo(
        site_dir: PathBuf,
        repo: site_repository::SiteRepository,
    ) -> Result<Self, String> {
        let conductor = Self::load_with_repo(site_dir, repo)?;
        conductor.migrate_suggestion_records();
        // Build all pages during startup so the output directory is populated immediately.
        conductor.build_all_pages(template::OutputMode::Editorial);
        Ok(conductor)
    }

    /// Everything [`Conductor::with_repo`] does except rendering pages.
    fn load_with_repo(
        site_dir: PathBuf,
        repo: site_repository::SiteRepository,
    ) -> Result<Self, String> {
        let site_dir = site_dir.canonicalize().unwrap_or(site_dir);
        let site_index = site_index::SiteIndex::new(site_dir.clone());

        // The conductor renders the working copy, unsaved edits included: it
        // writes the editorial output and never the published directory.
        let output_dir = site_index::editorial_output_dir(&site_dir);

        // Populate schema cache via repo
        let mut schema_cache = HashMap::new();
        for stem in repo.schema_stems() {
            if let Some(src) = repo.schema_source(&stem) {
                schema_cache.insert(stem.as_str().to_string(), src);
            }
            // Collection schemas keyed as "{stem}/index"
            if let Some(src) = repo.collection_schema_source(&stem) {
                schema_cache.insert(site_index::schema_cache_key(stem.as_str(), "index"), src);
            }
        }

        let conductor = Self {
            site_dir,
            output_dir,
            dep_graph: RwLock::new(dep_graph::DependencyGraph::new()),
            schema_cache: RwLock::new(schema_cache),
            parsed_grammars: RwLock::new(HashMap::new()),
            editors: RwLock::new(HashMap::new()),
            site_index: RwLock::new(site_index),
            repo,
            build_errors: RwLock::new(HashMap::new()),
            repopulate_failures: RwLock::new(Vec::new()),
            node_store: Arc::new(RwLock::new(node_store::NodeStore::new())),
            url_to_root: RwLock::new(HashMap::new()),
            url_to_semantic: RwLock::new(HashMap::new()),
            stem_to_roots: RwLock::new(HashMap::new()),
            cached_node_url_index: RwLock::new(HashMap::new()),
            cached_node_stem_index: RwLock::new(HashMap::new()),
            dirty_docs: RwLock::new(crate::dirty::DirtyDocs::new()),
            disk_stamps: RwLock::new(crate::dirty::DiskStamps::new()),
            disk_bases: RwLock::new(crate::dirty::DiskBases::new()),
            #[cfg(test)]
            after_read_hook: std::sync::Mutex::new(None),
            path_to_doc_root: RwLock::new(HashMap::new()),
            ned_suggestions: RwLock::new(HashMap::new()),
            self_write_tracker: crate::self_write_tracker::SelfWriteTracker::new(
                std::time::Duration::from_secs(5),
            ),
            stem_nodes: RwLock::new(HashMap::new()),
            validation_report: RwLock::new(validation::ValidationReport::default()),
        };

        // Load persisted NED suggestions from disk (read only: a load never
        // writes to `.presemble`, see `migrate_suggestion_records`)
        let ned_suggestions = Self::load_ned_suggestions(&conductor.ned_suggestions_dir());
        *conductor
            .ned_suggestions
            .write()
            .unwrap_or_else(|e| e.into_inner()) = ned_suggestions;

        // Populate the node store from the site repository
        conductor.populate_node_store(&[]);

        Ok(conductor)
    }

    /// The one-time changes to the suggestion records on disk. They run in
    /// [`Conductor::new`] and [`Conductor::with_repo`], the conductors that
    /// own the records, and never in [`Conductor::load`]: loading a site to
    /// validate it (what `presemble build` does) writes nothing to
    /// `.presemble`.
    ///
    /// First the legacy cleanup (ADR-047), then the model steps, which need
    /// the store.
    fn migrate_suggestion_records(&self) {
        // Delete any legacy sug-*.json files left over from before Wave C-ε
        // (ADR-047). Idempotent: later runs find nothing.
        Self::delete_legacy_suggestion_files(&self.site_dir);
        self.discard_suggestions_from_before_inline_nodes();
    }

    /// Once per workspace and step, drop the pending suggestions that touch a
    /// kind of block and were made before that kind held inline nodes
    /// (ADR-059 stages 4 and 5; [`MODEL_STEPS`]).
    ///
    /// Their text is markdown source and the new model reads text as literal,
    /// so applying them would write the markup as text. They are discarded
    /// silently: the file is deleted and nothing else is said but one log
    /// line per step. Accepted, rejected and stale records are history and
    /// stay.
    ///
    /// `.presemble/suggestions/ned/MODEL` holds the name of the last step
    /// applied. Every step after it runs, in order, and then the last one's
    /// name is written, so a suggestion made after a step is never discarded
    /// by it. An absent file means no step has run. It is written even when
    /// there is nothing to discard.
    fn discard_suggestions_from_before_inline_nodes(&self) {
        const MARKER: &str = "MODEL";
        let dir = self.ned_suggestions_dir();
        let marker = dir.join(MARKER);
        let recorded = std::fs::read_to_string(&marker).ok();
        let steps = model_steps_after(recorded.as_deref().map(str::trim));
        let Some(last) = steps.last() else {
            return;
        };

        for step in steps {
            let pending: Vec<editorial_types::NedSuggestion> = self
                .ned_suggestions
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|s| matches!(s.status, editorial_types::NedSuggestionStatus::Pending))
                .cloned()
                .collect();
            let mut discarded: Vec<String> = Vec::new();
            for suggestion in pending {
                if !step
                    .kinds
                    .iter()
                    .any(|kind| self.touches_kind(&suggestion, kind))
                {
                    continue;
                }
                let file = dir.join(format!("{}.json", suggestion.id));
                if let Err(e) = std::fs::remove_file(&file)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    eprintln!("conductor: could not delete {}: {e}", file.display());
                    continue; // kept: it would be loaded again, so it is not discarded
                }
                self.ned_suggestions
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&suggestion.id);
                discarded.push(suggestion.id.to_string());
            }
            if !discarded.is_empty() {
                discarded.sort();
                eprintln!(
                    "conductor: discarded {} pending suggestion(s) made before {}: {}",
                    discarded.len(),
                    step.before,
                    discarded.join(", ")
                );
            }
        }

        if let Err(e) =
            std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&marker, last.name))
        {
            eprintln!("conductor: could not write {}: {e}", marker.display());
        }
    }

    /// Whether `suggestion` changes a block of element kind `kind`: its
    /// payload holds one anywhere, or its selection resolves to one or to a
    /// node inside one. A selection that does not resolve is not counted: it
    /// cannot apply anyway, and on accept it goes Stale as unresolved.
    fn touches_kind(&self, suggestion: &editorial_types::NedSuggestion, kind: &str) -> bool {
        use editorial_types::NedMutation;
        fn holds_kind(tree: &ned::NodeTree, kind: &str) -> bool {
            match tree {
                ned::NodeTree::Element { name, children, .. } => {
                    name == kind || children.iter().any(|child| holds_kind(child, kind))
                }
                _ => false,
            }
        }
        let payload = match &suggestion.mutation {
            NedMutation::Replace(trees)
            | NedMutation::InsertChild(trees)
            | NedMutation::InsertBefore(trees)
            | NedMutation::InsertAfter(trees)
            | NedMutation::SetInlines(trees) => trees.iter().any(|tree| holds_kind(tree, kind)),
            NedMutation::SetText(_)
            | NedMutation::SearchReplace { .. }
            | NedMutation::SetComments { .. }
            | NedMutation::Delete => false,
        };
        if payload {
            return true;
        }
        let Ok(selection) = self.eval_selection(&suggestion.selection) else {
            return false;
        };
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let is_kind = |id: node_store::NodeId| matches!(store.get(id), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == kind);
        selection.iter().any(|id| {
            // The node and its structural ancestors (semantic collections are
            // not structure).
            let mut pending = vec![id];
            let mut seen = std::collections::HashSet::new();
            while let Some(node) = pending.pop() {
                if !seen.insert(node) {
                    continue;
                }
                if is_kind(node) {
                    return true;
                }
                pending.extend(
                    store
                        .parents(node)
                        .into_iter()
                        .filter(|p| !matches!(store.get(*p), Some(node_store::Node::Collection))),
                );
            }
            false
        })
    }

    /// The validation diagnostics from the latest population of the node
    /// store: every schema and content file of the site.
    pub fn validation_report(&self) -> validation::ValidationReport {
        self.validation_report
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn site_dir(&self) -> &Path {
        &self.site_dir
    }

    /// Git HEAD commit hash for the site directory.
    ///
    /// Returns `"untracked"` if the site isn't a git repo or any git error
    /// occurs (including `git` not being installed). No result is cached —
    /// git is fast and suggestions are created one at a time.
    pub fn workspace_hash(&self) -> String {
        use std::process::Command;
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.site_dir)
            .args(["rev-parse", "HEAD"])
            .output();
        match output {
            Ok(out) if out.status.success() => {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            }
            _ => "untracked".to_string(),
        }
    }

    /// Get a shared reference to the node store.
    pub fn node_store(&self) -> Arc<RwLock<node_store::NodeStore>> {
        Arc::clone(&self.node_store)
    }

    /// Access the NED dirty-docs tracker (Phase B edit path).
    pub fn dirty_docs(&self) -> &RwLock<crate::dirty::DirtyDocs> {
        &self.dirty_docs
    }

    /// Remember what the file at site-relative `rel` looks like now, as the
    /// version our copy is based on. Call it before reading the file (a
    /// change in between then shows as a change, never as a missed one), and
    /// after writing it.
    fn stamp_disk(&self, rel: &Path) {
        let stamp = crate::dirty::DiskStamp::of(&self.site_dir.join(rel));
        self.disk_stamps
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .record(rel, stamp);
    }

    /// Mark the documents at site-relative `paths` unsaved. One that goes from
    /// saved to unsaved gets the base of its edits (ADR-065 amendment F2).
    fn mark_unsaved(&self, paths: &[PathBuf]) {
        let newly_unsaved: Vec<&PathBuf> = {
            let mut dirty = self.dirty_docs.write().unwrap_or_else(|e| e.into_inner());
            paths
                .iter()
                .filter(|path| {
                    let new = !dirty.contains(path);
                    dirty.mark((*path).clone());
                    new
                })
                .collect()
        };
        for rel in newly_unsaved {
            self.record_base(rel);
        }
    }

    /// Remember the file at `rel` as the base of the document's unsaved edits,
    /// if it is still the version N was read from or last written as: the
    /// text and the stamp are read together, and the stamp must be the one
    /// recorded. Otherwise the document has no base, and a later disk change
    /// conflicts as a whole.
    fn record_base(&self, rel: &Path) {
        let Ok(site_path) = editorial_types::SitePath::parse(&rel.to_string_lossy()) else {
            return;
        };
        let Ok(abs) = crate::site_resolve::resolve_in_site(&self.site_dir, &site_path) else {
            return;
        };
        let Some((text, stamp)) = self.read_stable(&abs) else {
            return;
        };
        let agrees = self
            .disk_stamps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .matches(rel, stamp);
        if agrees {
            self.disk_bases
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .record(rel, text);
        }
    }

    /// The document at site-relative `rel` has no unsaved edits any more (it
    /// was saved, reloaded or settled): clear its dirty mark, any conflict,
    /// and the base of its edits.
    fn clear_unsaved(&self, rel: &Path) {
        self.dirty_docs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear(rel);
        self.disk_bases
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear(rel);
    }

    /// Write a document's text and remember the resulting file as the base of
    /// our copy. `rel` is site-relative; it is resolved inside the site (the
    /// one way a path becomes a file to write) and refused if it is not.
    fn write_document(&self, rel: &Path, text: &[u8]) -> std::io::Result<()> {
        let refuse = |e: editorial_types::PathError| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{}: {e}", rel.display()),
            )
        };
        let site_path = editorial_types::SitePath::parse(&rel.to_string_lossy()).map_err(refuse)?;
        let abs =
            crate::site_resolve::resolve_in_site(&self.site_dir, &site_path).map_err(refuse)?;
        crate::self_write_tracker::write_source_file(&abs, text, &self.self_write_tracker)?;
        self.stamp_disk(rel);
        Ok(())
    }

    /// Before a save: if the file of the dirty document at `rel` is not as we
    /// last saw it, something wrote it since, even if the watcher has not
    /// reported that yet. The change is taken as a disk change is
    /// ([`Conductor::take_disk_change`]); returns its events.
    fn detect_unreported_change(&self, rel: &Path) -> Vec<ConductorEvent> {
        let current = crate::dirty::DiskStamp::of(&self.site_dir.join(rel));
        let changed = self
            .disk_stamps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .changed(rel, current);
        if !changed {
            return Vec::new();
        }
        // A file that reads as what we hold is no conflict (the amendment to
        // ADR-065 §1): it is the version our copy is based on now (the check
        // records the stamp the text was read with).
        if self.disk_reads_as_n(rel) {
            return Vec::new();
        }
        self.take_disk_change(rel).unwrap_or_default()
    }

    /// The file of the document at site-relative `rel` changed on disk under
    /// its unsaved edits (ADR-065). The change is merged block by block into N
    /// against the base of the edits (amendment F2): what changed only on disk
    /// reaches N, and what changed differently on both sides conflicts on its
    /// blocks. Without a base, with the file gone, or when the document's
    /// structure changed, the whole file conflicts. Returns the events (pages
    /// rebuilt, and the conflict when it is new); `None` when the document has
    /// no unsaved edits.
    fn take_disk_change(&self, rel: &Path) -> Option<Vec<ConductorEvent>> {
        if !self
            .dirty_docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(rel)
        {
            return None;
        }
        Some(match self.merge_disk_change(rel) {
            editor_sync::DiskMerge::WholeDocument => self
                .record_disk_conflict(rel, Vec::new())
                .into_iter()
                .collect(),
            editor_sync::DiskMerge::Merged {
                mut events,
                regions,
            } => {
                if regions.is_empty() {
                    // Nothing conflicts (any more): the document is an
                    // ordinary unsaved document, with the merged blocks in N.
                    self.dirty_docs
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear_conflict(rel);
                } else {
                    events.extend(self.record_disk_conflict(rel, regions));
                }
                events
            }
        })
    }

    /// Record that the document at site-relative `rel` conflicts with its file
    /// on `regions` (none: the whole file). Returns the event to announce when
    /// the conflict is new.
    fn record_disk_conflict(
        &self,
        rel: &Path,
        regions: Vec<crate::protocol::ConflictRegion>,
    ) -> Option<ConductorEvent> {
        // A file that is gone still conflicts; its mtime is simply now.
        let disk_mtime = std::fs::metadata(self.site_dir.join(rel))
            .and_then(|m| m.modified())
            .unwrap_or_else(|_| std::time::SystemTime::now());
        self.dirty_docs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .mark_conflict(rel.to_path_buf(), disk_mtime, regions.clone())
            .then(|| ConductorEvent::DiskConflict {
                file: rel.to_string_lossy().to_string(),
                regions,
            })
    }

    /// The text of every dirty document (by site-relative path) except those in
    /// `disk_wins`, serialized from the store as it is now.
    ///
    /// A dirty document that cannot be serialized is left out, so its disk
    /// version is loaded; that is recorded as a build error on its page (see
    /// [`Conductor::flush_repopulate_failures`]) rather than dropped silently.
    fn unsaved_documents(&self, disk_wins: &[PathBuf]) -> HashMap<PathBuf, String> {
        let dirty: Vec<PathBuf> = self
            .dirty_docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(Path::to_path_buf)
            .filter(|path| !disk_wins.contains(path))
            .collect();
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let mut failures = self
            .repopulate_failures
            .write()
            .unwrap_or_else(|e| e.into_inner());
        dirty
            .into_iter()
            .filter_map(|path| {
                let root = self.doc_root_for_path(&path)?;
                match node_store_bridge::serialize_from_store(&store, root) {
                    Ok(text) => Some((path, text)),
                    Err(e) => {
                        let url =
                            node_store_bridge::content_bridge::find_attr_text(&store, root, "url")
                                .unwrap_or_else(|| path.to_string_lossy().to_string());
                        failures.push((
                            url,
                            format!(
                                "unsaved edits could not be kept: {e}; the file on disk was loaded"
                            ),
                        ));
                        None
                    }
                }
            })
            .collect()
    }

    /// Record the build errors left by the last repopulation (unsaved edits
    /// that could not be kept) and return the pages they belong to, for the
    /// `BuildFailed` event.
    fn flush_repopulate_failures(&self) -> Vec<String> {
        let failures = std::mem::take(
            &mut *self
                .repopulate_failures
                .write()
                .unwrap_or_else(|e| e.into_inner()),
        );
        let mut errors = self.build_errors.write().unwrap_or_else(|e| e.into_inner());
        let mut pages = Vec::new();
        for (url, message) in failures {
            errors.entry(url.clone()).or_default().push(message);
            if !pages.contains(&url) {
                pages.push(url);
            }
        }
        pages
    }

    /// Populate the node store from the site repository.
    /// Walks schemas, content, and templates, converting them to nodes/edges.
    ///
    /// One rule for every repopulation: every dirty document is serialized from
    /// the store before it is cleared and re-ingested from that text, so
    /// unsaved edits survive. `disk_wins` lists the site-relative paths that
    /// are the exception: their file on disk is loaded instead (a file that
    /// changed on disk, or was just created).
    fn populate_node_store(&self, disk_wins: &[PathBuf]) {
        let keep = self.unsaved_documents(disk_wins);
        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
        store.clear();

        // Use a fresh repo to discover current files (self.repo may be stale after scaffold/create-content)
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();

        let mut url_index: HashMap<String, node_store::NodeId> = HashMap::new();
        let mut semantic_index: HashMap<String, node_store::NodeId> = HashMap::new();
        let mut stem_index = StemRoots::new();

        // Validation diagnostics for every schema and content file (ADR-061).
        let mut report = validation::ValidationReport::default();
        let relative = |path: PathBuf| -> PathBuf {
            path.strip_prefix(&self.site_dir)
                .map(Path::to_path_buf)
                .unwrap_or(path)
        };
        // Site-relative path of the schema stored under schema cache `key`.
        let schema_file = |key: &str| -> PathBuf {
            let path = if key == "index" {
                repo.collection_schema_path(&site_index::SchemaStem::new(""))
            } else if let Some(stem) = key.strip_suffix("/index") {
                repo.collection_schema_path(&site_index::SchemaStem::new(stem))
            } else {
                repo.schema_path(&site_index::SchemaStem::new(key))
            };
            relative(path)
        };

        // Parse and store all schemas
        let schema_cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
        let mut grammars: HashMap<String, schema::Grammar> = HashMap::new();
        // Schema cache keys whose schema failed to parse.
        let mut failed_schemas: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for (stem_key, src) in schema_cache.iter() {
            match schema::parse_schema(src) {
                Ok(grammar) => {
                    node_store_bridge::schema_bridge::grammar_to_store(&grammar, &mut store);
                    grammars.insert(stem_key.clone(), grammar);
                }
                Err(e) => {
                    report.push(validation::SourceDiagnostic {
                        file: schema_file(stem_key),
                        at: None,
                        severity: validation::Severity::Error,
                        message: e.to_string(),
                    });
                    failed_schemas.insert(stem_key.clone());
                }
            }
        }
        drop(schema_cache);

        // The "not built" error for a content file with no usable grammar,
        // given the schema cache keys tried for it, in lookup order.
        let not_built = |file: PathBuf, keys: &[&str]| -> validation::SourceDiagnostic {
            let message = match keys.iter().find(|k| failed_schemas.contains(**k)) {
                Some(key) => format!(
                    "not built: schema `{}` does not parse",
                    schema_file(key).display()
                ),
                None => "not built: no schema for this page".to_string(),
            };
            validation::SourceDiagnostic {
                file,
                at: None,
                severity: validation::Severity::Error,
                message,
            }
        };
        // Diagnostics for parsed (or unparseable) content at `file`.
        let content_diagnostics = |report: &mut validation::ValidationReport,
                                   file: &Path,
                                   src: &str,
                                   parsed: &Result<content::Document, content::ContentError>,
                                   grammar: &schema::Grammar| {
            match parsed {
                Ok(doc) => {
                    for d in validation::validate_document(src, doc, grammar) {
                        report.push(validation::SourceDiagnostic {
                            file: file.to_path_buf(),
                            at: d.span.map(|s| validation::line_col(src, s.start)),
                            severity: d.severity,
                            message: d.message,
                        });
                    }
                }
                Err(e) => report.push(validation::SourceDiagnostic {
                    file: file.to_path_buf(),
                    at: None,
                    severity: validation::Severity::Error,
                    message: format!("parse error: {e}"),
                }),
            }
        };

        // Pass 1: Parse and store all content documents (no semantic content yet)
        // We need all documents indexed before we can resolve link expressions.
        struct DocEntry {
            url: String,
            root: node_store::NodeId,
            grammar_key: String,
            meta: node_store_bridge::content_bridge::DocumentMeta,
        }
        let mut doc_entries: Vec<DocEntry> = Vec::new();

        for stem in repo.schema_stems() {
            let stem_str = stem.as_str();

            // Item content: the files there are, and the documents with
            // unsaved edits whose file is gone (ADR-065): those stay, as a
            // conflict, until they are saved (which writes the file again) or
            // reloaded.
            let mut slugs = repo.content_slugs(&stem);
            for kept_path in keep.keys() {
                if let Some(slug) = kept_path.file_stem().and_then(|s| s.to_str())
                    && slug != "index"
                    && !slugs.iter().any(|s| s == slug)
                    && relative(repo.content_path(&stem, slug)) == *kept_path
                {
                    slugs.push(slug.to_string());
                }
            }
            for slug in slugs {
                let rel = relative(repo.content_path(&stem, &slug));
                let kept = keep.get(&rel).cloned();
                if kept.is_none() {
                    self.stamp_disk(&rel);
                }
                if let Some(src) = kept.or_else(|| repo.content_source(&stem, &slug)) {
                    let grammar_key = format!("{stem_str}/item");
                    let grammar = grammars
                        .get(&grammar_key)
                        .or_else(|| grammars.get(stem_str));
                    let file = relative(repo.content_path(&stem, &slug));
                    let Some(grammar) = grammar else {
                        report.push(not_built(file, &[&grammar_key, stem_str]));
                        continue;
                    };
                    let parsed = content::parse_and_assign(&src, grammar);
                    content_diagnostics(&mut report, &file, &src, &parsed, grammar);
                    if let Ok(doc) = parsed {
                        let slug_str = slug.as_str();
                        let url = site_index::url_for_stem_slug(stem_str, slug_str);
                        let file = site_index::content_file_path(stem_str, slug_str);
                        let meta = node_store_bridge::content_bridge::DocumentMeta {
                            url: url.clone(),
                            stem: stem_str.to_string(),
                            file,
                            page_kind: "item".to_string(),
                        };
                        let root = node_store_bridge::content_bridge::document_to_store(
                            &doc,
                            &mut store,
                            Some(&meta),
                        );
                        url_index.insert(url.clone(), root);
                        stem_index
                            .entry(stem_str.to_string())
                            .or_default()
                            .insert(url.clone(), root);
                        doc_entries.push(DocEntry {
                            url,
                            root,
                            grammar_key,
                            meta,
                        });
                    }
                }
            }

            // Collection content
            let rel = relative(repo.collection_content_path(&stem));
            let kept = keep.get(&rel).cloned();
            if kept.is_none() {
                self.stamp_disk(&rel);
            }
            if let Some(src) = kept.or_else(|| repo.collection_content_source(&stem)) {
                let grammar_key = site_index::schema_cache_key(stem_str, "index");
                let file = relative(repo.collection_content_path(&stem));
                // A broken collection schema does not fall back to the item
                // grammar: the fallback is only for stems with no collection
                // schema at all.
                let collection_schema_broken = failed_schemas.contains(&grammar_key);
                let grammar = if collection_schema_broken {
                    None
                } else {
                    grammars
                        .get(&grammar_key)
                        .or_else(|| grammars.get(stem_str))
                };
                let parsed = match grammar {
                    Some(grammar) => {
                        let parsed = content::parse_and_assign(&src, grammar);
                        content_diagnostics(&mut report, &file, &src, &parsed, grammar);
                        parsed.ok()
                    }
                    None if collection_schema_broken => {
                        report.push(not_built(file, &[&grammar_key]));
                        None
                    }
                    None => {
                        report.push(not_built(file, &[&grammar_key, stem_str]));
                        None
                    }
                };
                if let Some(doc) = parsed {
                    let url = site_index::url_for_stem_slug(stem_str, "index");
                    let file = site_index::content_file_path(stem_str, "index");
                    let meta = node_store_bridge::content_bridge::DocumentMeta {
                        url: url.clone(),
                        stem: stem_str.to_string(),
                        file,
                        page_kind: "collection".to_string(),
                    };
                    let root = node_store_bridge::content_bridge::document_to_store(
                        &doc,
                        &mut store,
                        Some(&meta),
                    );
                    url_index.insert(url.clone(), root);
                    stem_index
                        .entry(stem_str.to_string())
                        .or_default()
                        .insert(url.clone(), root);
                    doc_entries.push(DocEntry {
                        url,
                        root,
                        grammar_key,
                        meta,
                    });
                }
            }
        }

        // Pass 2: Create semantic content. Process items before collections
        // so that collection pages' link slots can resolve to item semantic roots.
        doc_entries.sort_by_key(|e| {
            if e.meta.page_kind == "collection" {
                1
            } else {
                0
            }
        });
        let stem_lists = stem_root_lists(&stem_index);
        for entry in &doc_entries {
            if let Some(grammar) = grammars
                .get(&entry.grammar_key)
                .or_else(|| grammars.get(&entry.meta.stem))
            {
                let sem = node_store_bridge::content_bridge::create_semantic_content(
                    &mut store,
                    entry.root,
                    grammar,
                    &entry.meta,
                    &stem_lists,
                    &url_index,
                    Some(&semantic_index),
                );
                semantic_index.insert(entry.url.clone(), sem);
            }
        }

        // Parse and store all templates
        for stem in repo.schema_stems() {
            if let Some((src, is_hiccup)) = repo.item_template_source(&stem) {
                let nodes = if is_hiccup {
                    template::parse_template_hiccup(&src).ok()
                } else {
                    template::parse_template_xml(&src).ok()
                };
                if let Some(nodes) = nodes {
                    node_store_bridge::template_bridge::template_to_store(&nodes, &mut store);
                }
            }
            if let Some((src, is_hiccup)) = repo.collection_template_source(&stem) {
                let nodes = if is_hiccup {
                    template::parse_template_hiccup(&src).ok()
                } else {
                    template::parse_template_xml(&src).ok()
                };
                if let Some(nodes) = nodes {
                    node_store_bridge::template_bridge::template_to_store(&nodes, &mut store);
                }
            }
        }

        // Walk the templates directory for partial templates not tied to a schema stem
        let templates_dir = self.site_dir.join("templates");
        if templates_dir.is_dir() {
            Self::walk_and_store_templates(&templates_dir, &mut store);
        }

        // Phase 1c legacy fallback root: if no root URL is registered and a
        // `templates/index.html` (or `.hiccup`) exists, create a synthetic empty
        // document node so that `build_all_pages` will render the root index page.
        if !url_index.contains_key("/") {
            let root_stem = site_index::SchemaStem::new("");
            if repo.collection_template_source(&root_stem).is_some() {
                // Create a minimal document node matching the structure that
                // document_to_store and store_to_document expect.
                let doc_name = store.intern("document");
                let root = store.add_node(node_store::Node::Element(doc_name));
                // Required metadata attributes
                let url_name = store.intern("url");
                let url_val = store.add_node(node_store::Node::Text("/".to_string()));
                store.add_edge(
                    root,
                    node_store::Edge::Attribute {
                        name: url_name,
                        value: url_val,
                    },
                );
                let stem_name = store.intern("stem");
                let stem_val = store.add_node(node_store::Node::Text(String::new()));
                store.add_edge(
                    root,
                    node_store::Edge::Attribute {
                        name: stem_name,
                        value: stem_val,
                    },
                );
                let file_name = store.intern("file");
                let file_val = store.add_node(node_store::Node::Text(String::new()));
                store.add_edge(
                    root,
                    node_store::Edge::Attribute {
                        name: file_name,
                        value: file_val,
                    },
                );
                let pk_name = store.intern("page-kind");
                let pk_val = store.add_node(node_store::Node::Text("collection".to_string()));
                store.add_edge(
                    root,
                    node_store::Edge::Attribute {
                        name: pk_name,
                        value: pk_val,
                    },
                );
                let sep_name = store.intern("has-separator");
                let sep_val = store.add_node(node_store::Node::Boolean(false));
                store.add_edge(
                    root,
                    node_store::Edge::Attribute {
                        name: sep_name,
                        value: sep_val,
                    },
                );
                // Required structural children: empty preamble and body
                let preamble_name = store.intern("preamble");
                let preamble = store.add_node(node_store::Node::Element(preamble_name));
                store.add_edge(root, node_store::Edge::Child(preamble));
                let body_name = store.intern("body");
                let body = store.add_node(node_store::Node::Element(body_name));
                store.add_edge(root, node_store::Edge::Child(body));
                url_index.insert("/".to_string(), root);
                stem_index
                    .entry(String::new())
                    .or_default()
                    .insert("/".to_string(), root);

                // Create semantic content for this synthetic collection page.
                // Phase C needs all renderable pages in url_to_semantic.
                let sem_name = store.intern("semantic-content");
                let sem = store.add_node(node_store::Node::Element(sem_name));

                let stem_attr_name = store.intern("stem");
                let stem_attr_val = store.add_node(node_store::Node::Text(String::new()));
                store.add_edge(
                    sem,
                    node_store::Edge::Attribute {
                        name: stem_attr_name,
                        value: stem_attr_val,
                    },
                );

                let pk_attr_name = store.intern("page-kind");
                let pk_attr_val = store.add_node(node_store::Node::Text("collection".to_string()));
                store.add_edge(
                    sem,
                    node_store::Edge::Attribute {
                        name: pk_attr_name,
                        value: pk_attr_val,
                    },
                );

                let url_attr_name = store.intern("url");
                let url_attr_val = store.add_node(node_store::Node::Text("/".to_string()));
                store.add_edge(
                    sem,
                    node_store::Edge::Attribute {
                        name: url_attr_name,
                        value: url_attr_val,
                    },
                );

                semantic_index.insert("/".to_string(), sem);
            }
        }

        // Build path_to_doc_root from doc_entries (file attr → root NodeId)
        let path_index: HashMap<PathBuf, node_store::NodeId> = doc_entries
            .iter()
            .map(|e| (PathBuf::from(&e.meta.file), e.root))
            .collect();

        // Commit indexes (drop store lock first to avoid write-write deadlock)
        drop(store);
        *self.url_to_root.write().unwrap_or_else(|e| e.into_inner()) = url_index;
        *self
            .url_to_semantic
            .write()
            .unwrap_or_else(|e| e.into_inner()) = semantic_index;
        *self
            .stem_to_roots
            .write()
            .unwrap_or_else(|e| e.into_inner()) = stem_index;
        *self
            .path_to_doc_root
            .write()
            .unwrap_or_else(|e| e.into_inner()) = path_index;
        *self
            .validation_report
            .write()
            .unwrap_or_else(|e| e.into_inner()) = report.sorted();

        // Query nodes, stem nodes and preamble fingerprints for the fresh store.
        self.establish_derived_state();
    }

    /// The parsed grammar for a page: the schema under
    /// `schema_cache_key(stem, slug)`, falling back to the stem's item schema
    /// (as `populate_node_store` does). Parsed on demand.
    pub(crate) fn grammar_for_page(&self, stem: &str, slug: &str) -> Option<schema::Grammar> {
        let key = site_index::schema_cache_key(stem, slug);
        let src = self
            .schema_source(&key)
            .or_else(|| self.schema_source(stem))?;
        schema::parse_schema(&src).ok()
    }

    /// `(stem, slug)` of the page whose document root is `doc_root`, read from
    /// its `stem` and `file` attributes. `None` for a root without a file
    /// (the synthetic root index).
    fn page_identity(
        store: &node_store::NodeStore,
        doc_root: node_store::NodeId,
    ) -> Option<(String, String)> {
        let stem = node_store_bridge::content_bridge::find_attr_text(store, doc_root, "stem")?;
        let file = node_store_bridge::content_bridge::find_attr_text(store, doc_root, "file")?;
        let slug = Path::new(&file).file_stem()?.to_str()?.to_string();
        Some((stem, slug))
    }

    /// Snapshot of the live URL indexes, for building a `ThreadContext`.
    fn index_snapshots(
        &self,
    ) -> (
        HashMap<String, node_store::NodeId>,
        HashMap<String, node_store::NodeId>,
    ) {
        let sem = self
            .url_to_semantic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let roots = self
            .url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (sem, roots)
    }

    /// Resolve the link expressions of page `url` against `ctx`
    /// (`store_pipeline::resolve_page_link_expressions`), with the page's
    /// grammar. `grammars` memoises parsed grammars within one call site.
    fn resolve_page(
        &self,
        store: &mut node_store::NodeStore,
        ctx: &node_store_bridge::store_pipeline::ThreadContext,
        url: &str,
        grammars: &mut HashMap<(String, String), Option<schema::Grammar>>,
    ) {
        let (Some(&sem), Some(&root)) = (ctx.url_to_semantic.get(url), ctx.url_to_root.get(url))
        else {
            return;
        };
        let Some((stem, slug)) = Self::page_identity(store, root) else {
            return;
        };
        let key = (stem.clone(), site_index::schema_cache_key(&stem, &slug));
        let grammar = grammars
            .entry(key)
            .or_insert_with(|| self.grammar_for_page(&stem, &slug));
        if let Some(grammar) = grammar {
            node_store_bridge::store_pipeline::resolve_page_link_expressions(
                store, sem, root, url, grammar, ctx,
            );
        }
    }

    /// The stem node for `stem`, created (and indexed) if missing.
    fn ensure_stem_node(
        &self,
        store: &mut node_store::NodeStore,
        stem: &str,
    ) -> node_store::NodeId {
        let mut stem_nodes = self.stem_nodes.write().unwrap_or_else(|e| e.into_inner());
        if let Some(&node) = stem_nodes.get(stem)
            && store.get(node).is_some()
        {
            return node;
        }
        let node = node_store_bridge::store_pipeline::create_stem_node(store, stem);
        stem_nodes.insert(stem.to_string(), node);
        node
    }

    /// Build the derived state for a freshly populated store: stem nodes, the
    /// thread context, every page's resolved link expressions with their
    /// query nodes, and every document's preamble fingerprint.
    fn establish_derived_state(&self) {
        let (url_to_semantic, url_to_root) = self.index_snapshots();
        let schema_stems: Vec<String> = self
            .schema_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|k| !k.contains('/') && *k != "index")
            .cloned()
            .collect();

        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
        self.stem_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Every document stem, schema stem and thread source gets a stem
        // node, so every query node has exactly one `over` edge.
        let mut stems: std::collections::BTreeSet<String> = schema_stems.into_iter().collect();
        for &root in url_to_root.values() {
            if let Some(stem) =
                node_store_bridge::content_bridge::find_attr_text(&store, root, "stem")
            {
                stems.insert(stem);
            }
            stems.extend(
                node_store_bridge::store_pipeline::preamble_threads(&store, root)
                    .into_iter()
                    .map(|t| t.source),
            );
        }
        for stem in &stems {
            self.ensure_stem_node(&mut store, stem);
        }
        let stem_nodes = self
            .stem_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        for &root in url_to_root.values() {
            let fp = node_store_bridge::store_pipeline::preamble_fingerprint(&store, root);
            node_store_bridge::store_pipeline::store_fingerprint(&mut store, root, fp);
        }

        let mut urls: Vec<String> = url_to_semantic.keys().cloned().collect();
        urls.sort();
        let ctx = node_store_bridge::store_pipeline::ThreadContext::build(
            &store,
            url_to_semantic,
            url_to_root,
            stem_nodes,
        );
        let mut grammars = HashMap::new();
        for url in &urls {
            self.resolve_page(&mut store, &ctx, url, &mut grammars);
        }
    }

    /// Propagate document changes through the link-expression queries.
    ///
    /// Returns the URLs to render: every changed document, plus each page
    /// whose link-expression result changes or contains a changed document.
    /// Resolves (store mutation) but does not render.
    ///
    /// For each change X:
    /// 1. X's preamble fingerprint is computed and stored (no context needed).
    ///    A body-only edit (X existed and the fingerprint is unchanged)
    ///    affects no query.
    /// 2. The thread context is built, in full, only when some change needs
    ///    it: its preamble changed, or it has thread expressions of its own
    ///    (a re-ingested root's thread slots must be re-resolved).
    /// 3. X is always rendered; when it needs the context, its own queries
    ///    are resolved first.
    /// 4. When X's preamble changed, every query over X's stem is checked;
    ///    its page is re-resolved and rendered when the result changed or X
    ///    is in the old or new result (D3a).
    pub(crate) fn propagate_document_changes(
        &self,
        changes: &[DocChange],
    ) -> std::collections::BTreeSet<String> {
        use node_store_bridge::store_pipeline as sp;

        let mut render = std::collections::BTreeSet::new();
        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());

        // 1. Fingerprints first; they need no context.
        struct Pending<'a> {
            change: &'a DocChange,
            preamble_changed: bool,
            needs_ctx: bool,
        }
        let mut pending = Vec::with_capacity(changes.len());
        for change in changes {
            render.insert(change.url.clone());
            let old_fp =
                sp::fingerprint_of(&store, change.previous_root.unwrap_or(change.doc_root));
            let new_fp = sp::preamble_fingerprint(&store, change.doc_root);
            sp::store_fingerprint(&mut store, change.doc_root, new_fp);
            let preamble_changed = old_fp != Some(new_fp);
            let threads = sp::preamble_threads(&store, change.doc_root);
            for thread in &threads {
                self.ensure_stem_node(&mut store, &thread.source);
            }
            self.ensure_stem_node(&mut store, &change.stem);
            let needs_ctx = preamble_changed || !threads.is_empty();
            pending.push(Pending {
                change,
                preamble_changed,
                needs_ctx,
            });
        }

        // 2. The context, only if some change needs it.
        if !pending.iter().any(|p| p.needs_ctx) {
            return render;
        }
        let (url_to_semantic, url_to_root) = self.index_snapshots();
        let stem_nodes = self
            .stem_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let ctx = sp::ThreadContext::build(&store, url_to_semantic, url_to_root, stem_nodes);

        let mut grammars = HashMap::new();
        let mut to_resolve = std::collections::BTreeSet::new();
        for p in pending.iter().filter(|p| p.needs_ctx) {
            // 3. X's own queries.
            self.resolve_page(&mut store, &ctx, &p.change.url, &mut grammars);
            if !p.preamble_changed {
                // Body-only edit: nothing a query reads has changed.
                continue;
            }

            // 4. Queries over X's stem.
            let x_url = sp::normalise_url(&p.change.url);
            let Some(&stem_node) = ctx.stem_nodes.get(&p.change.stem) else {
                continue;
            };
            for q in sp::queries_over(&store, stem_node) {
                let old = sp::query_result(&store, q);
                let Some((page, thread)) = sp::query_thread(&store, &ctx, q) else {
                    continue;
                };
                let has_take = thread
                    .ops
                    .iter()
                    .any(|op| matches!(op, content::LinkOp::Take(_)));
                // D3a, the conservative interim rule: when X is in the
                // result, its page re-renders even if the result is
                // unchanged. The goal is that a change to a referenced page
                // triggers a more thorough analysis (which fields P's
                // template reads) that may or may not result in a re-render;
                // the D3a rule is the conservative interim.
                let affected = if has_take {
                    let new = sp::reevaluate_query(&store, &ctx, q);
                    new.as_ref() != Some(&old)
                        || old.contains(&x_url)
                        || new.as_ref().is_some_and(|n| n.contains(&x_url))
                } else {
                    // Without `take` the result is the candidates that pass
                    // the filters (`filter`, `refs-to`), in a stable sort
                    // order. X's edit changes neither whether any other item
                    // passes (X's own references feed `refs-to` only through
                    // X itself, as a source) nor the relative order of any two
                    // other items. So "the result changed" implies X is in the
                    // old or the new result: in the old one (stored) or
                    // admitted now (`thread_admits` on X alone).
                    old.contains(&x_url) || sp::thread_admits(&store, &ctx, &thread, &page, &x_url)
                };
                if affected {
                    to_resolve.insert(page);
                }
            }
        }
        for page in to_resolve {
            self.resolve_page(&mut store, &ctx, &page, &mut grammars);
            render.insert(page);
        }
        render
    }

    /// The stored result of every link-expression query, keyed by
    /// `(page, slot)`. A local value for before/after comparisons; not stored.
    pub(crate) fn snapshot_query_results(&self) -> node_store_bridge::store_pipeline::QueryResults {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem_nodes = self.stem_nodes.read().unwrap_or_else(|e| e.into_inner());
        node_store_bridge::store_pipeline::snapshot_query_results(&store, stem_nodes.values())
    }

    /// The declared-reference index over every document in the store
    /// (`store_pipeline::links_to_index`).
    pub fn links_to(&self) -> expressions::LinksToIndex {
        let docs: Vec<(String, node_store::NodeId)> = self
            .url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(u, &r)| (u.clone(), r))
            .collect();
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        node_store_bridge::store_pipeline::links_to_index(&store, &docs)
    }

    fn walk_and_store_templates(dir: &std::path::Path, store: &mut node_store::NodeStore) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    Self::walk_and_store_templates(&path, store);
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && matches!(ext, "hiccup" | "html")
                    && let Ok(src) = std::fs::read_to_string(&path)
                {
                    let nodes = if ext == "hiccup" {
                        template::parse_template_hiccup(&src).ok()
                    } else {
                        template::parse_template_xml(&src).ok()
                    };
                    if let Some(nodes) = nodes {
                        node_store_bridge::template_bridge::template_to_store(&nodes, store);
                    }
                }
            }
        }
    }

    /// Get cached schema source for a stem.
    pub fn schema_source(&self, stem: &str) -> Option<String> {
        self.schema_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(stem)
            .cloned()
    }

    /// Look up a document root NodeId by URL path.
    pub fn document_by_url(&self, url: &str) -> Option<node_store::NodeId> {
        self.url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(url)
            .copied()
    }

    /// Resolve the canonical schema URL for a given content page URL.
    ///
    /// Canonicalizes the URL (strips trailing `index.html`/`index.htm`, tries with/without
    /// trailing slash), looks up the page in `url_to_root`, reads `stem` and `page-kind`
    /// attributes, and constructs the `/_schema/<stem>/<index|item>` URL.
    ///
    /// Returns `None` if the page is not found in the NodeStore.
    pub fn schema_url_for_page(&self, page_url: &str) -> Option<String> {
        // Canonicalize: strip trailing index.html / index.htm, then normalize slashes.
        let stripped = page_url
            .trim_end_matches("index.html")
            .trim_end_matches("index.htm");
        let base = stripped.trim_end_matches('/');

        // Build the with-slash and without-slash candidates.
        let with_slash = if base.is_empty() {
            "/".to_string()
        } else {
            format!("{base}/")
        };
        let without_slash = base.to_string();

        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
        let root_id = url_to_root
            .get(&with_slash)
            .or_else(|| url_to_root.get(&without_slash))
            .or_else(|| url_to_root.get(page_url))
            .copied()?;
        drop(url_to_root);

        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, root_id, "stem")?;
        let page_kind =
            node_store_bridge::content_bridge::find_attr_text(&store, root_id, "page-kind")?;
        drop(store);

        let kind_segment = match page_kind.as_str() {
            "collection" => "index",
            _ => "item",
        };

        let schema_url = if stem.is_empty() {
            format!("/_schema/{kind_segment}")
        } else {
            format!("/_schema/{stem}/{kind_segment}")
        };
        Some(schema_url)
    }

    /// Resolve a best-effort representative content page URL for a given schema URL.
    ///
    /// Parses the `/_schema/<stem>/<index|item>` path to extract `(stem, kind)`, then:
    /// - For `index`/root: returns `Some("/")`
    /// - For `index`/named stem: returns `Some("/<stem>/")` if that collection page exists
    /// - For `item`: walks all roots for the stem, picks the first item URL in URL order
    ///
    /// Returns `None` for malformed or unknown schema URLs.
    pub fn page_url_for_schema(&self, schema_url: &str) -> Option<String> {
        let (stem, kind) = site_index::parse_schema_url(schema_url)?;

        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());

        match kind {
            schema::SchemaKind::Index => {
                let url = collection_url(&stem);
                // The root collection page always exists.
                (stem.is_empty() || url_to_root.contains_key(&url)).then_some(url)
            }
            schema::SchemaKind::Item => {
                // Try the parent collection page first
                let url = collection_url(&stem);
                if url_to_root.contains_key(&url) {
                    return Some(url);
                }

                let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                let roots = self.documents_for_stem(&stem);

                // First item in URL order (documents_for_stem is URL-ordered).
                roots.iter().find_map(|&root| {
                    let pk = node_store_bridge::content_bridge::find_attr_text(
                        &store,
                        root,
                        "page-kind",
                    )?;
                    if pk != "item" {
                        return None;
                    }
                    node_store_bridge::content_bridge::find_attr_text(&store, root, "url")
                })
            }
        }
    }

    /// Look up a document root NodeId by content file path.
    ///
    /// The path is relative to the site root (e.g. `content/post/first.md`),
    /// matching the `file` attribute stored on document root nodes.
    #[allow(dead_code)] // consumed by B3
    pub(crate) fn doc_root_for_path(&self, content_path: &Path) -> Option<node_store::NodeId> {
        self.path_to_doc_root
            .read()
            .ok()?
            .get(content_path)
            .copied()
    }

    /// Look up a semantic content NodeId by URL path.
    pub fn semantic_content_by_url(&self, url: &str) -> Option<node_store::NodeId> {
        self.url_to_semantic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(url)
            .copied()
    }

    /// Get all document root NodeIds for a given schema stem, in URL order.
    pub fn documents_for_stem(&self, stem: &str) -> Vec<node_store::NodeId> {
        self.stem_to_roots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(stem)
            .map(|roots| roots.values().copied().collect())
            .unwrap_or_default()
    }

    /// Materialize a DataGraph from a document's NodeStore subtree.
    /// Uses the pass-through approach: reconstruct Document from nodes,
    /// then call the existing build_article_graph.
    pub fn datagraph_for_document(
        &self,
        doc_root: node_store::NodeId,
    ) -> Option<template::DataGraph> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());

        // Get the stem attribute to find the grammar
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "stem")?;
        let url = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "url")?;
        let page_kind =
            node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "page-kind");
        // Derive _presemble_file before releasing the store lock.
        let presemble_file =
            node_store_bridge::content_bridge::presemble_file_for_root(&store, doc_root);

        // Reconstruct the Document from the NodeStore
        let doc = match node_store_bridge::content_bridge::store_to_document(&store, doc_root) {
            Ok(doc) => doc,
            Err(e) => {
                eprintln!("conductor: cannot decode document {url}: {e}");
                return None;
            }
        };
        drop(store);

        // Find the grammar
        let schema_cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
        let is_collection = page_kind.as_deref() == Some("collection");
        let grammar_key = if is_collection {
            site_index::schema_cache_key(&stem, "index")
        } else {
            // Try stem/item first, then just stem
            let item_key = format!("{stem}/item");
            if schema_cache.contains_key(&item_key) {
                item_key
            } else {
                stem.clone()
            }
        };

        let grammar_src = schema_cache.get(&grammar_key).cloned();
        drop(schema_cache);

        // Build the DataGraph using the existing function.
        // For template-only pages (no associated schema), produce an empty DataGraph
        // so the template can still render with injected collection data.
        let mut data = if let Some(ref src) = grammar_src
            && let Ok(grammar) = schema::parse_schema(src)
        {
            template::build_article_graph(&doc, &grammar, template::OutputMode::Editorial)
        } else {
            // No grammar (template-only page like the legacy root index).
            // Return an empty DataGraph; collection injection in build_all_pages
            // will populate it with article lists as needed.
            template::DataGraph::new()
        };

        // Inject metadata (same as site_builder does)
        data.insert("_presemble_stem", template::Value::Text(stem.clone()));
        // Always insert _presemble_file. Synthesised from the document root's
        // :file attribute (with stem fallback) by the shared helper, computed
        // above before releasing the store lock.
        data.insert("_presemble_file", template::Value::Text(presemble_file));
        data.insert("url", template::Value::Text(url.clone()));

        // Synthesize link record
        let title = match data.resolve(&["title"]) {
            Some(v @ (template::Value::Text(_) | template::Value::Inlines(_))) => {
                v.display_text().unwrap_or_default()
            }
            _ => url.split('/').next_back().unwrap_or("").to_string(),
        };
        let link = template::synthesize_link(&title, &url, template::title_editor(&data));
        data.insert("link", template::Value::Record(link));

        Some(data)
    }

    /// Query all item documents for a stem, returning materialized DataGraphs.
    /// This is the NodeStore equivalent of the SiteGraph-based query_items_for_stem.
    pub fn query_items_from_store(&self, stem: &str) -> Vec<(String, template::DataGraph)> {
        self.documents_for_stem(stem)
            .into_iter()
            .filter_map(|root| {
                let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                let page_kind =
                    node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                if page_kind != "item" {
                    return None;
                }
                let url = node_store_bridge::content_bridge::find_attr_text(&store, root, "url")?;
                drop(store); // release read lock before calling datagraph_for_document
                let data = self.datagraph_for_document(root)?;
                Some((url, data))
            })
            .collect()
    }

    /// Get a snapshot of the url→root index (URL string → NodeId).
    /// Used by evaluator primitives.
    pub fn url_to_root_index(&self) -> HashMap<String, node_store::NodeId> {
        self.url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// List all content item URL paths (page-kind=item) from the NodeStore.
    pub fn list_content_urls(&self) -> Vec<String> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let url_to_root = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
        let mut urls: Vec<String> = url_to_root
            .iter()
            .filter_map(|(url, &root)| {
                let pk =
                    node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                if pk == "item" {
                    Some(url.clone())
                } else {
                    None
                }
            })
            .collect();
        urls.sort();
        urls
    }

    /// Build the expression indexes (UrlIndex, StemIndex, LinksToIndex) from
    /// the NodeStore, for evaluating link expressions at the REPL.
    pub fn build_expression_indexes_from_store_pub(
        &self,
    ) -> (
        expressions::UrlIndex,
        expressions::StemIndex,
        expressions::LinksToIndex,
    ) {
        self.build_expression_indexes_from_store()
    }

    fn build_expression_indexes_from_store(
        &self,
    ) -> (
        expressions::UrlIndex,
        expressions::StemIndex,
        expressions::LinksToIndex,
    ) {
        // Collect all (stem, url, root) tuples for item documents without holding locks
        let items: Vec<(String, String, node_store::NodeId)> = {
            let stem_to_roots = self.stem_to_roots.read().unwrap_or_else(|e| e.into_inner());
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            stem_to_roots
                .iter()
                .flat_map(|(stem_str, roots)| {
                    roots
                        .values()
                        .filter_map(|&root| {
                            let page_kind = node_store_bridge::content_bridge::find_attr_text(
                                &store,
                                root,
                                "page-kind",
                            );
                            if page_kind.as_deref() != Some("item") {
                                return None;
                            }
                            let url = node_store_bridge::content_bridge::find_attr_text(
                                &store, root, "url",
                            )?;
                            Some((stem_str.clone(), url, root))
                        })
                        .collect::<Vec<_>>()
                })
                .collect()
        };

        // Materialize DataGraphs — each call acquires its own locks
        let mut url_index: expressions::UrlIndex = std::collections::HashMap::new();
        let mut stem_index: expressions::StemIndex = std::collections::HashMap::new();

        for (stem_str, url, root) in &items {
            if let Some(data) = self.datagraph_for_document(*root) {
                let url_path = site_index::UrlPath::new(url);
                let schema_stem = site_index::SchemaStem::new(stem_str);

                url_index.insert(url_path.clone(), data.clone());
                stem_index
                    .entry(schema_stem)
                    .or_default()
                    .push((url_path, data));
            }
        }

        (url_index, stem_index, self.links_to())
    }

    /// Get cached NodeStore-native indexes for incremental page rebuilds.
    fn cached_node_indexes(
        &self,
    ) -> (
        node_store_bridge::store_pipeline::NodeUrlIndex,
        node_store_bridge::store_pipeline::NodeStemIndex,
    ) {
        let url = self
            .cached_node_url_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let stem = self
            .cached_node_stem_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        (url, stem)
    }

    /// Every `(url, semantic root)` pair, URL ascending, so collection
    /// injection and rendering see the same order on every build.
    fn semantic_pairs_by_url(&self) -> Vec<(String, node_store::NodeId)> {
        let mut pairs: Vec<(String, node_store::NodeId)> = self
            .url_to_semantic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(url, &sem)| (url.clone(), sem))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    /// Batch-render all pages in the site.
    ///
    /// Replaces the O(n²) pattern of `render_pages` (which called
    /// `build_expression_indexes_from_store` once per page) with a single O(n)
    /// pipeline where each phase visits every page exactly once.
    ///
    /// `presemble build` renders [`template::OutputMode::Published`] pages,
    /// which carry no editor-only data (ADR-063); serve renders editorial
    /// pages.
    ///
    /// Returns `(rebuilt_pages, failed_pages, errors)`.
    pub fn build_all_pages(
        &self,
        output: template::OutputMode,
    ) -> (Vec<String>, Vec<String>, HashMap<String, Vec<String>>) {
        // === NodeStore-native pipeline (Phase B) ===
        // Run NodeStore operations first to enrich the store.
        // The legacy DataGraph pipeline below still runs for rendering.
        // TODO: Phase C — use NodeStoreView for rendering, remove DataGraph pipeline.
        {
            let semantic_pairs = self.semantic_pairs_by_url();

            if !semantic_pairs.is_empty() {
                let (node_url_index, node_stem_index) = {
                    let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                    node_store_bridge::store_pipeline::build_indexes_from_store(
                        &store,
                        &semantic_pairs,
                    )
                };

                // Link expressions are already resolved (establish_derived_state
                // at the end of populate_node_store); this pass only renders.
                let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
                for (_url, sem_root) in &semantic_pairs {
                    node_store_bridge::store_pipeline::resolve_cross_references_in_store(
                        &mut store,
                        *sem_root,
                        &node_url_index,
                    );
                }
                node_store_bridge::store_pipeline::inject_collections_in_store(
                    &mut store,
                    &semantic_pairs,
                    &node_stem_index,
                );

                // Cache NodeStore-native indexes for rebuild_page
                *self
                    .cached_node_url_index
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = node_url_index;
                *self
                    .cached_node_stem_index
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = node_stem_index;
            }
        }
        // === End NodeStore-native pipeline ===

        // Phase 2f: Apply templates and write output (one pass).
        // Uses NodeStoreView via PrefixedGraphView — no DataGraph materialization.
        let fresh_repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let registry = template_registry::FileTemplateRegistry::new(fresh_repo.clone());

        // Iterate over semantic content roots instead of DataGraphs.
        let semantic_pairs = self.semantic_pairs_by_url();
        // The document of each page, for the diagnostics of editorial pages.
        let url_roots = self
            .url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());

        // Render all pages in parallel — each page writes to a unique file, no shared mutable state.
        let results: Vec<PageRenderResult> = semantic_pairs
            .par_iter()
            .filter_map(|(url, sem_root)| {
                // Read stem from semantic content attributes
                let stem =
                    node_store_bridge::content_bridge::find_attr_text(&store, *sem_root, "stem")
                        .unwrap_or_default();
                if stem.is_empty() && url != "/" {
                    return None;
                }

                let page_kind = node_store_bridge::content_bridge::find_attr_text(
                    &store,
                    *sem_root,
                    "page-kind",
                );

                let slug = if url.ends_with('/') || url == "/" {
                    "index"
                } else {
                    url.rsplit('/').next().unwrap_or("index")
                };

                // Find template
                let stem_obj = site_index::SchemaStem::new(&stem);
                let template_result =
                    if slug == "index" || page_kind.as_deref() == Some("collection") {
                        fresh_repo
                            .collection_template_source(&stem_obj)
                            .or_else(|| fresh_repo.item_template_source(&stem_obj))
                            .or_else(|| fresh_repo.partial_template_source(&stem))
                    } else {
                        fresh_repo
                            .item_template_source(&stem_obj)
                            .or_else(|| fresh_repo.partial_template_source(&stem))
                    };

                let (tmpl_src, is_hiccup) = match template_result {
                    Some(t) => t,
                    None => {
                        if stem.is_empty() {
                            return None;
                        }
                        return Some((
                            url.clone(),
                            Err((url.clone(), vec![format!("no template for {stem}")])),
                        ));
                    }
                };

                let raw_nodes = if is_hiccup {
                    match template::parse_template_hiccup(&tmpl_src) {
                        Ok(n) => n,
                        Err(e) => {
                            return Some((url.clone(), Err((url.clone(), vec![format!("{e}")]))));
                        }
                    }
                } else {
                    match template::parse_template_xml(&tmpl_src) {
                        Ok(n) => n,
                        Err(e) => {
                            return Some((url.clone(), Err((url.clone(), vec![format!("{e}")]))));
                        }
                    }
                };

                let (nodes, local_defs) = template::extract_definitions(raw_nodes);
                let ctx = template::RenderContext::with_local_defs(&registry, &local_defs, output);

                // Use PrefixedGraphView — templates access data via input.field paths
                let view = node_store_bridge::NodeStoreView::new(&store, *sem_root);
                let prefixed = node_store_bridge::PrefixedGraphView::new("input".to_string(), view);

                match template::transform(nodes, &prefixed, &ctx) {
                    Ok(transformed) => {
                        // Editorial pages carry their diagnostics where they
                        // arise; published pages carry nothing editor-only.
                        let html = match (output, url_roots.get(url)) {
                            (template::OutputMode::Editorial, Some(&doc_root)) => {
                                self.editorial_html(&store, doc_root, transformed)
                            }
                            _ => template::serialize_nodes(&transformed),
                        };
                        let output_path =
                            site_index::output_path_for_stem_slug(&self.output_dir, &stem, slug);
                        if let Some(parent) = output_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = std::fs::write(&output_path, &html);
                        Some((url.clone(), Ok(())))
                    }
                    Err(e) => Some((
                        url.clone(),
                        Err((url.clone(), vec![format!("render error: {e}")])),
                    )),
                }
            })
            .collect();

        drop(store);

        // Merge parallel results into the output vecs.
        let mut rebuilt_pages = Vec::new();
        let mut failed_pages = Vec::new();
        let mut errors: HashMap<String, Vec<String>> = HashMap::new();

        for (url, result) in results {
            match result {
                Ok(()) => rebuilt_pages.push(url),
                Err((fail_url, errs)) => {
                    failed_pages.push(fail_url.clone());
                    errors.insert(fail_url, errs);
                }
            }
        }

        (rebuilt_pages, failed_pages, errors)
    }

    /// Refresh the schema cache by re-scanning the filesystem.
    /// Called after scaffolding or when schema files change on disk.
    fn refresh_schema_cache(&self) {
        // Re-create the repo from filesystem to discover new schemas
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let mut cache = self.schema_cache.write().unwrap_or_else(|e| e.into_inner());
        cache.clear();
        for stem in repo.schema_stems() {
            if let Some(src) = repo.schema_source(&stem) {
                cache.insert(stem.as_str().to_string(), src);
            }
            // Collection schemas keyed as "{stem}/index" (or "index" for root)
            if let Some(src) = repo.collection_schema_source(&stem) {
                cache.insert(site_index::schema_cache_key(stem.as_str(), "index"), src);
            }
        }
    }

    /// Refresh the site index by re-creating it from the filesystem.
    /// Called after scaffolding or after new content directories are created.
    fn refresh_site_index(&self) {
        *self.site_index.write().unwrap_or_else(|e| e.into_inner()) =
            site_index::SiteIndex::new(self.site_dir.clone());
    }

    /// Insert a URL→NodeId mapping into the conductor's url_to_root index.
    /// Used in tests to populate the NodeStore without going through the full
    /// site-repository pipeline.
    pub fn insert_url_root(&self, url: &str, root: node_store::NodeId) {
        self.url_to_root
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), root);
        // Also update stem index and path index if the node has the required attributes
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, root, "stem");
        let file = node_store_bridge::content_bridge::find_attr_text(&store, root, "file");
        drop(store);
        if let Some(stem) = stem {
            self.stem_to_roots
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(stem)
                .or_default()
                .insert(url.to_string(), root);
        }
        if let Some(file) = file {
            self.path_to_doc_root
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(PathBuf::from(file), root);
        }
    }

    /// Insert a URL→semantic NodeId mapping.
    /// Used in tests alongside insert_url_root.
    pub fn insert_url_semantic(&self, url: &str, sem: node_store::NodeId) {
        self.url_to_semantic
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), sem);
    }

    /// Return all item data graphs for a given schema stem.
    ///
    /// Returns a vec of `(url_path, data_graph)` pairs, one per item page.
    pub fn query_items_for_stem(&self, stem: &str) -> Vec<(String, template::DataGraph)> {
        self.query_items_from_store(stem)
    }

    /// Return all edges pointing TO the given URL path: one per page that
    /// declares a reference to it (`store_pipeline::links_to_index`), sorted
    /// by source.
    pub fn query_edges_to(&self, target_url: &str) -> Vec<site_index::Edge> {
        let target = node_store_bridge::store_pipeline::normalise_url(target_url);
        let mut sources: Vec<String> = self
            .links_to()
            .remove(&target)
            .unwrap_or_default()
            .into_iter()
            .collect();
        sources.sort();
        sources
            .into_iter()
            .map(|source| site_index::Edge {
                source: site_index::UrlPath::new(&source),
                target: site_index::UrlPath::new(&target),
            })
            .collect()
    }

    /// Return all edges originating FROM the given URL path: one per page it
    /// declares a reference to (`store_pipeline::links_to_index`), sorted by
    /// target.
    pub fn query_edges_from(&self, source_url: &str) -> Vec<site_index::Edge> {
        let source = node_store_bridge::store_pipeline::normalise_url(source_url);
        let mut targets: Vec<String> = self
            .links_to()
            .into_iter()
            .filter(|(_, sources)| sources.contains(&source))
            .map(|(target, _)| target)
            .collect();
        targets.sort();
        targets
            .into_iter()
            .map(|target| site_index::Edge {
                source: site_index::UrlPath::new(&source),
                target: site_index::UrlPath::new(&target),
            })
            .collect()
    }

    /// The text of a document on disk.
    ///
    /// A path that resolves outside the site is not found.
    pub fn document_text(&self, path: &editorial_types::SitePath) -> Option<String> {
        let abs = crate::site_resolve::resolve_in_site(&self.site_dir, path).ok()?;
        std::fs::read_to_string(abs).ok()
    }

    /// [`Conductor::document_text`] for a path the conductor built itself.
    fn document_text_at(&self, abs: &Path) -> Option<String> {
        let path = editorial_types::SitePath::from_absolute(&self.site_dir, abs).ok()?;
        self.document_text(&path)
    }

    /// List all link completion options for a given schema stem.
    ///
    /// Reads from the NodeStore and extracts title from the document's preamble.
    /// Falls back to the slug if no title is found.
    pub fn list_link_options(&self, stem: &str) -> Vec<crate::protocol::LinkOption> {
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let roots = self.documents_for_stem(stem);
        let mut options: Vec<crate::protocol::LinkOption> = roots
            .into_iter()
            .filter_map(|root| {
                // Only item documents (not collections)
                let page_kind =
                    node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind")?;
                if page_kind != "item" {
                    return None;
                }

                let url = node_store_bridge::content_bridge::find_attr_text(&store, root, "url")?;
                let slug = url
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string();

                // Get title from the document's preamble
                let title = find_document_title(&store, root).unwrap_or_else(|| slug.clone());

                Some(crate::protocol::LinkOption {
                    stem: stem.to_string(),
                    slug,
                    title,
                    url,
                })
            })
            .collect();
        options.sort_by(|a, b| a.slug.cmp(&b.slug));
        options
    }

    /// For a slot on a source schema, derive the target collection stem
    /// by inspecting the slot's link-target href pattern. Returns None if
    /// the slot isn't a link slot or the target can't be resolved.
    pub fn resolve_link_target_stem(&self, source_stem: &str, slot: &str) -> Option<String> {
        // 1. Look up the source schema source from cache
        let src = self.schema_source(source_stem)?;
        // 2. Parse the schema
        let grammar = schema::parse_schema(&src).ok()?;
        // 3. Find the slot by name in preamble
        let found_slot = grammar.preamble.iter().find(|s| s.name.as_str() == slot)?;
        // 4. Check if it's a Link element with a pattern
        let pattern = match &found_slot.element {
            schema::Element::Link { pattern } => pattern,
            schema::Element::Image { pattern } => pattern,
            _ => return None,
        };
        // 5. Extract stem from pattern like "/stem/<placeholder>"
        // Skip external URLs
        if pattern.starts_with("http://") || pattern.starts_with("https://") {
            return None;
        }
        // Remove fragment and query parts
        let path = pattern.split('#').next().unwrap_or(pattern);
        let path = path.split('?').next().unwrap_or(path);
        // Split on '/' and find the first non-empty, non-placeholder segment
        let stem = path
            .split('/')
            .find(|seg| !seg.is_empty() && !seg.contains('<') && !seg.contains('>'))?;
        Some(stem.to_string())
    }

    /// List all schema stems known to the conductor (excludes collection schemas).
    /// Whether `stem` names a schema of this site (`""` is the root
    /// collection). A stem from a client is checked here before it is used
    /// to build anything.
    pub fn is_known_stem(&self, stem: &str) -> bool {
        stem.is_empty()
            || self
                .schema_cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .keys()
                .any(|key| key.split('/').next() == Some(stem))
    }

    pub fn list_schemas(&self) -> Vec<String> {
        let cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
        let mut stems: Vec<String> = cache
            .keys()
            .filter(|k| !k.contains('/')) // exclude collection schemas like "post/index"
            .cloned()
            .collect();
        stems.sort();
        stems
    }

    /// Render one page to the output directory from the NodeStore.
    ///
    /// `doc_root` is the document root NodeId (from `url_to_root` / `doc_root_for_path`).
    /// The store must already contain up-to-date, resolved content for this
    /// page (see `propagate_document_changes`); this function neither
    /// re-ingests source text nor evaluates link expressions.
    ///
    /// Returns the list of URL paths that were rendered, or an error string.
    pub(crate) fn render_page_output(
        &self,
        doc_root: node_store::NodeId,
    ) -> Result<Vec<String>, String> {
        // Read stem, url, and file from the document root's attributes
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let stem = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "stem")
            .ok_or_else(|| format!("doc root {doc_root:?} has no stem attribute"))?;
        let url_path = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "url")
            .ok_or_else(|| format!("doc root {doc_root:?} has no url attribute"))?;
        let file_attr = node_store_bridge::content_bridge::find_attr_text(&store, doc_root, "file")
            .ok_or_else(|| format!("doc root {doc_root:?} has no file attribute"))?;
        drop(store);

        // Derive slug from the file attribute (e.g. "content/post/first.md" → "first")
        let slug = Path::new(&file_attr)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| format!("cannot derive slug from file attribute: {file_attr}"))?
            .to_string();

        // Look up the semantic root (needed for rendering)
        let sem_root = self
            .url_to_semantic
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&url_path)
            .copied()
            .ok_or_else(|| format!("no semantic root for url {url_path}"))?;

        // Run NodeStore-native pipeline on this page's semantic content
        {
            let (node_url_index, node_stem_index) = self.cached_node_indexes();
            let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
            node_store_bridge::store_pipeline::resolve_cross_references_in_store(
                &mut store,
                sem_root,
                &node_url_index,
            );
            // Inject collections: build from cached stem index
            // We need ALL semantic roots for collection injection
            let all_roots = self.semantic_pairs_by_url();
            node_store_bridge::store_pipeline::inject_collections_in_store(
                &mut store,
                &all_roots,
                &node_stem_index,
            );
        }

        // Load and parse template
        let fresh_repo = site_repository::SiteRepository::builder()
            .from_dir(&self.site_dir)
            .build();
        let stem_obj = site_index::SchemaStem::new(&stem);
        let page_kind = {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            node_store_bridge::content_bridge::find_attr_text(&store, sem_root, "page-kind")
        };
        let (tmpl_src, is_hiccup) =
            if slug == "index" || page_kind.as_deref() == Some("collection") {
                fresh_repo
                    .collection_template_source(&stem_obj)
                    .or_else(|| fresh_repo.item_template_source(&stem_obj))
                    .or_else(|| fresh_repo.partial_template_source(&stem))
            } else {
                fresh_repo
                    .item_template_source(&stem_obj)
                    .or_else(|| fresh_repo.partial_template_source(&stem))
            }
            .ok_or_else(|| format!("no template for {stem}"))?;

        let raw_nodes = if is_hiccup {
            template::parse_template_hiccup(&tmpl_src).map_err(|e| format!("{e}"))?
        } else {
            template::parse_template_xml(&tmpl_src).map_err(|e| format!("{e}"))?
        };
        let (nodes, local_defs) = template::extract_definitions(raw_nodes);

        let registry = template_registry::FileTemplateRegistry::new(fresh_repo);
        let ctx = template::RenderContext::with_local_defs(
            &registry,
            &local_defs,
            template::OutputMode::Editorial,
        );

        // Render via PrefixedGraphView — same as build_all_pages Phase 2f
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let view = node_store_bridge::NodeStoreView::new(&store, sem_root);
        let prefixed = node_store_bridge::PrefixedGraphView::new("input".to_string(), view);

        let transformed = template::transform(nodes, &prefixed, &ctx)
            .map_err(|e| format!("render error: {e}"))?;
        // Editorial pages carry their diagnostics where they arise.
        let html = self.editorial_html(&store, doc_root, transformed);
        drop(store);

        // Write output
        let output_path = site_index::output_path_for_stem_slug(&self.output_dir, &stem, &slug);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        std::fs::write(&output_path, &html).map_err(|e| format!("write error: {e}"))?;

        Ok(vec![url_path])
    }

    /// Rebuild pages for a batch of content file paths (relative to site root)
    /// whose nodes were mutated in place (NED, block edit,
    /// suggestion accept).
    ///
    /// Each path's document is a change for `propagate_document_changes`
    /// (with no previous root: the fingerprint stored on the live root is the
    /// old value); every URL it returns is rendered once. Unknown paths are
    /// silently skipped.
    ///
    /// Returns `(rebuilt_urls, failed_urls)`.
    pub(crate) fn rebuild_pages_for_modified_nodes(
        &self,
        paths: &[PathBuf],
    ) -> (Vec<String>, Vec<String>) {
        let mut changes = Vec::new();
        let mut path_of_url: HashMap<String, &PathBuf> = HashMap::new();
        {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            for path in paths {
                let Some(root) = self.doc_root_for_path(path) else {
                    continue; // not in store — skip silently
                };
                let url = node_store_bridge::content_bridge::find_attr_text(&store, root, "url");
                let stem = node_store_bridge::content_bridge::find_attr_text(&store, root, "stem");
                if let (Some(url), Some(stem)) = (url, stem) {
                    path_of_url.insert(url.clone(), path);
                    changes.push(DocChange {
                        url,
                        stem,
                        doc_root: root,
                        previous_root: None,
                    });
                }
            }
        }

        let urls = self.propagate_document_changes(&changes);
        let mut rebuilt = Vec::new();
        let mut failed = Vec::new();
        for url in urls {
            match self.render_url(&url) {
                Ok(urls) => rebuilt.extend(urls),
                Err(e) => {
                    // A changed document is reported by its path stem (best
                    // effort, as before); a dependent page by its URL.
                    let reported = match path_of_url.get(&url) {
                        Some(path) => {
                            eprintln!("conductor: rebuild failed for {}: {e}", path.display());
                            path.file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("unknown")
                                .to_string()
                        }
                        None => {
                            eprintln!("conductor: rebuild failed for {url}: {e}");
                            url
                        }
                    };
                    failed.push(reported);
                }
            }
        }

        (rebuilt, failed)
    }

    /// Render the page at `url` (`render_page_output`).
    fn render_url(&self, url: &str) -> Result<Vec<String>, String> {
        let root = self
            .document_by_url(url)
            .ok_or_else(|| format!("no doc root for {url}"))?;
        self.render_page_output(root)
    }

    /// Where the content file at `content_path` (site-relative) sits: its
    /// schema stem, slug and URL, and the grammar of its schema.
    fn document_coordinates(
        &self,
        content_path: &Path,
    ) -> Result<(String, String, String, schema::Grammar), String> {
        let stem = match self
            .site_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .classify(content_path)
        {
            site_index::FileKind::Content { schema_stem } => schema_stem.to_string(),
            _ => return Err(format!("not a content file: {}", content_path.display())),
        };
        let slug = content_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let schema_key = site_index::schema_cache_key(&stem, &slug);
        let schema_src = self
            .schema_source(&schema_key)
            .ok_or_else(|| format!("no schema for {schema_key}"))?;
        let grammar =
            schema::parse_schema(&schema_src).map_err(|e| format!("schema error: {e:?}"))?;
        let url = site_index::url_for_stem_slug(&stem, &slug);
        Ok((stem, slug, url, grammar))
    }

    /// Recompute the references of a page's semantic content from the store's
    /// current content, in place: the semantic node and the document root keep
    /// their ids, nothing is parsed or serialized. Nothing is rendered.
    ///
    /// A block that an edit removed or replaced took its incoming edges with
    /// it, among them the page's semantic references to it; filling the same
    /// node again recomputes them (ADR-059 §6). Whatever points at the
    /// semantic node (a listing page) keeps pointing at it.
    ///
    /// The fill has no thread context, so a thread slot comes out as every
    /// item of its stem, unsorted and unlimited. The thread slots are
    /// evaluated again by `propagate_document_changes`, which
    /// `rebuild_pages_for_modified_nodes` runs right after this: a page with
    /// thread expressions builds the context and re-resolves its own queries
    /// (`resolve_page`). Pinned by
    /// `structural_edit_on_a_page_with_a_thread_slot_keeps_its_listing`.
    ///
    /// Returns the URLs of the other pages that point at this page's semantic
    /// content: they show it, so they must render again.
    fn refresh_semantic_content(&self, content_path: &Path) -> Result<Vec<String>, String> {
        let (stem, slug, url, grammar) =
            self.document_coordinates(&self.site_dir.join(content_path))?;
        let doc_root = self
            .doc_root_for_path(content_path)
            .ok_or_else(|| format!("no document in the store for {}", content_path.display()))?;
        let sem = self
            .semantic_content_by_url(&url)
            .ok_or_else(|| format!("no semantic content for {url}"))?;
        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: url.clone(),
            stem: stem.clone(),
            file: site_index::content_file_path(&stem, &slug),
            page_kind: if slug == "index" {
                "collection"
            } else {
                "item"
            }
            .to_string(),
        };
        let stem_roots =
            stem_root_lists(&self.stem_to_roots.read().unwrap_or_else(|e| e.into_inner()));
        let (url_to_semantic, url_to_root) = self.index_snapshots();

        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
        node_store_bridge::content_bridge::clear_semantic_content(&mut store, sem);
        node_store_bridge::content_bridge::fill_semantic_content(
            &mut store,
            sem,
            doc_root,
            &grammar,
            &meta,
            &stem_roots,
            &url_to_root,
            Some(&url_to_semantic),
        );

        // The pages that point at this semantic node, directly or through a
        // collection of references.
        let mut dependents = Vec::new();
        let mut seen = std::collections::HashSet::from([sem]);
        let mut queue = vec![sem];
        while let Some(node) = queue.pop() {
            for (source, edge) in store.incoming(node) {
                if matches!(edge, node_store::Edge::Derived { .. }) || !seen.insert(source) {
                    continue;
                }
                match node_store_bridge::content_bridge::find_attr_text(&store, source, "url") {
                    Some(page) => dependents.push(page),
                    None => queue.push(source),
                }
            }
        }
        dependents.sort();
        dependents.dedup();
        Ok(dependents)
    }

    /// Rebuild a single content page from in-memory text.
    ///
    /// Re-ingests `text` into the NodeStore via `update_document_in_store`,
    /// propagates the change (`propagate_document_changes`) and renders every
    /// page it returns: the page itself plus the pages whose link-expression
    /// output changes or contains it.
    ///
    /// Returns the list of URL paths that were rebuilt, or an error string
    /// when the page itself fails. A dependent page that fails to render is
    /// logged and left out. Errors here are non-fatal: the caller logs and
    /// continues.
    fn rebuild_page(&self, content_path: &Path, text: &str) -> Result<Vec<String>, String> {
        let (stem, slug, url_path, grammar) = self.document_coordinates(content_path)?;
        let slug = slug.as_str();

        // The root this re-ingest supersedes (it keeps its fingerprint).
        let previous_root = self
            .path_to_doc_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(Path::new(&site_index::content_file_path(&stem, slug)))
            .copied();

        // Update document in NodeStore (incremental — re-parses and creates semantic content)
        self.update_document_in_store(&url_path, &stem, slug, text, &grammar)?;

        // Look up the doc root (update_document_in_store just wrote it)
        let doc_root = self
            .url_to_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&url_path)
            .copied()
            .ok_or_else(|| format!("no doc root for {url_path} after update"))?;

        let urls = self.propagate_document_changes(&[DocChange {
            url: url_path.clone(),
            stem,
            doc_root,
            previous_root,
        }]);

        let mut rebuilt = self.render_page_output(doc_root)?;
        for url in urls.into_iter().filter(|u| *u != url_path) {
            match self.render_url(&url) {
                Ok(pages) => rebuilt.extend(pages),
                Err(e) => eprintln!("conductor: rebuild failed for dependent page {url}: {e}"),
            }
        }
        Ok(rebuilt)
    }

    /// Re-parse a single document and update its semantic content in the NodeStore.
    /// This is the incremental counterpart to `populate_node_store`.
    ///
    /// Steps:
    /// 1. Parse the document text with the grammar
    /// 2. Store the document in the NodeStore (replacing old nodes)
    /// 3. Create new semantic content with resolved link expressions
    /// 4. Update url_to_root, url_to_semantic, stem_to_roots, path_to_doc_root
    ///
    /// Returns the semantic root NodeId.
    fn update_document_in_store(
        &self,
        url: &str,
        stem: &str,
        slug: &str,
        text: &str,
        grammar: &schema::Grammar,
    ) -> Result<node_store::NodeId, String> {
        let page_kind = if slug == "index" {
            "collection"
        } else {
            "item"
        };
        let file = site_index::content_file_path(stem, slug);

        let doc =
            content::parse_and_assign(text, grammar).map_err(|e| format!("parse error: {e}"))?;

        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: url.to_string(),
            stem: stem.to_string(),
            file,
            page_kind: page_kind.to_string(),
        };

        // Not atomic with the index updates below: a concurrent re-ingest of the
        // same file can leave two live roots. Wave 1 swaps store and indexes
        // atomically.
        let old_root = self
            .path_to_doc_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(Path::new(&meta.file))
            .copied();

        let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());

        // ADR-056 exact addressing: retire the superseded root by changing its
        // kind to `superseded-document`, so every `:kind "document"` query
        // (`doc-by-path`, `by-url`, ...) resolves to exactly one document. Its
        // attributes and edges are kept: other pages' semantic Reference edges
        // still point at it and must keep reading a correct `url` / `file`.
        // Reclaiming the old subtree is left to garbage collection (Wave 1).
        if let Some(old_root) = old_root {
            Self::retire_document_root(&mut store, old_root);
        }

        // Store the document (creates new nodes; the old subtree stays behind
        // as a retired, superseded root, see above)
        let root =
            node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));

        // Build stem_to_roots and url_to_root snapshots for link resolution.
        // We need ALL documents' roots so link expressions can resolve cross-document.
        let url_to_root_snapshot: HashMap<String, node_store::NodeId> = {
            let existing = self.url_to_root.read().unwrap_or_else(|e| e.into_inner());
            let mut map: HashMap<String, node_store::NodeId> =
                existing.iter().map(|(k, &v)| (k.clone(), v)).collect();
            map.insert(url.to_string(), root);
            map
        };

        // Keyed by URL: inserting replaces this URL's old root in place, so
        // the stem's documents stay in URL order.
        let stem_to_roots_snapshot: StemRoots = {
            let mut map = self
                .stem_to_roots
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            map.entry(stem.to_string())
                .or_default()
                .insert(url.to_string(), root);
            map
        };

        // Create semantic content with resolved link expressions.
        // The write lock on node_store must be held during this call.
        // Pass url_to_semantic so links resolve to semantic roots when available
        let sem_snapshot: std::collections::HashMap<String, node_store::NodeId> = {
            self.url_to_semantic
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(k, &v)| (k.clone(), v))
                .collect()
        };
        let sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            root,
            grammar,
            &meta,
            &stem_root_lists(&stem_to_roots_snapshot),
            &url_to_root_snapshot,
            Some(&sem_snapshot),
        );

        drop(store);

        // Update indexes
        self.url_to_root
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), root);

        self.url_to_semantic
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(url.to_string(), sem);

        {
            let mut stem_roots = self
                .stem_to_roots
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let roots = stem_roots.entry(stem.to_string()).or_default();
            // The snapshot already has the correct roots (this URL's root
            // replaced). Replace the live entry with the snapshot value.
            *roots = stem_to_roots_snapshot
                .get(stem)
                .cloned()
                .unwrap_or_default();
        }

        // Update path_to_doc_root
        self.path_to_doc_root
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(PathBuf::from(&meta.file), root);

        Ok(sem)
    }

    /// Retire a superseded document root by renaming its element kind from
    /// `document` to [`SUPERSEDED_DOCUMENT_KIND`].
    ///
    /// The superseded root drops out of every `:kind "document"` query
    /// (`doc-by-path`, `by-url`, `by-stem`, `posts`, `all-documents`,
    /// `source-docs-of`, and `content_store::edit::resolve`). All attributes and
    /// edges are kept, so stale references from other pages still read a
    /// correct `url` / `file`. Wave 1 reclaims the subtree.
    ///
    /// No-op if `root` is not a `document` element.
    fn retire_document_root(store: &mut node_store::NodeStore, root: node_store::NodeId) {
        if !matches!(
            store.get(root),
            Some(node_store::Node::Element(name)) if store.resolve_name(*name) == DOCUMENT_KIND
        ) {
            return;
        }
        let kind = store.intern(SUPERSEDED_DOCUMENT_KIND);
        store.replace_node(root, node_store::Node::Element(kind));
    }

    /// Map a cursor line to the structural anchor of the nearest body element.
    ///
    /// Returns `None` if the document cannot be parsed or has no relevant elements.
    fn body_element_anchor_at_line(
        &self,
        src: &str,
        path: &str,
        line: u32,
    ) -> Option<editorial_types::StructuralAnchor> {
        // Derive schema stem from path.
        // "content/post/my-post.md" → "post"
        // "content/index.md" or "content/hello.md" → "" (root collection)
        let stem = {
            let rest = path.strip_prefix("content/")?;
            // If there's another '/' it's a subdir → stem is the part before '/'
            // Otherwise it's a root-level file → stem is ""
            if let Some(slash_pos) = rest.find('/') {
                &rest[..slash_pos]
            } else {
                ""
            }
        };

        // Load grammar from cache
        let schema_src = self.schema_source(stem)?;
        let grammar = schema::parse_schema(&schema_src).ok()?;

        // Parse and assign slots
        let doc = content::parse_and_assign(src, &grammar).ok()?;

        let byte_offset = line_to_byte_offset(src, line);

        // Skip preamble — preamble elements don't have id attributes in the
        // rendered HTML yet, so scrolling to them would silently fail.
        // TODO: add id="presemble-slot-{name}" to rendered preamble elements,
        // then re-enable preamble scroll.

        // Check body elements — exact match
        for (idx, spanned) in doc.body.iter().enumerate() {
            if spanned.span.start <= byte_offset && byte_offset < spanned.span.end {
                return body_idx_to_structural(&doc, path, idx);
            }
        }

        // Cursor might be between elements — find the nearest body element
        if doc.has_separator && !doc.body.is_empty() {
            let mut closest_idx = 0;
            let mut closest_dist = usize::MAX;
            for (idx, spanned) in doc.body.iter().enumerate() {
                let dist = if byte_offset < spanned.span.start {
                    spanned.span.start - byte_offset
                } else {
                    byte_offset - spanned.span.end
                };
                if dist < closest_dist {
                    closest_dist = dist;
                    closest_idx = idx;
                }
            }
            return body_idx_to_structural(&doc, path, closest_idx);
        }

        None
    }

    /// Compute a [`StructuralAnchor`] for a body element at `body_idx` in `file`.
    ///
    /// Reads the document from the in-memory buffer (or disk), parses it, and
    /// delegates to [`body_idx_to_structural`].  Returns `None` if the document
    /// cannot be found/parsed or the index is out of range.
    fn compute_body_anchor(
        &self,
        file: &str,
        body_idx: usize,
    ) -> Option<editorial_types::StructuralAnchor> {
        // Derive schema stem from path: content/{stem}/file.md → stem, content/file.md → ""
        let bpath = std::path::Path::new(file);
        let bcomponents: Vec<_> = bpath.components().collect();
        let stem = if bcomponents.len() == 2 {
            String::new()
        } else {
            bcomponents.get(1)?.as_os_str().to_str()?.to_string()
        };

        let bslug = bpath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = site_index::schema_cache_key(&stem, bslug);
        let schema_src = self.schema_source(&schema_key)?;
        let grammar = schema::parse_schema(&schema_src).ok()?;

        let abs_path = self.site_dir.join(file);
        let src = self.document_text_at(&abs_path)?;
        let doc = content::parse_and_assign(&src, &grammar).ok()?;

        body_idx_to_structural(&doc, file, body_idx)
    }

    // ── NED suggestion persistence helpers ───────────────────────────────────────

    /// Path to the `.presemble/suggestions/ned/` directory.
    fn ned_suggestions_dir(&self) -> PathBuf {
        self.site_dir
            .join(".presemble")
            .join("suggestions")
            .join("ned")
    }

    /// Delete legacy top-level `sug-*.json` files from `.presemble/suggestions/`.
    ///
    /// These were written by the pre-Wave-C-ε suggestion system (ADR-047).
    /// The `ned/` subdirectory is left untouched — those are active NED suggestions.
    /// Idempotent: if no files exist (or the directory doesn't exist), this is a no-op.
    fn delete_legacy_suggestion_files(site_dir: &Path) {
        let dir = site_dir.join(".presemble").join("suggestions");
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return, // No suggestions dir; nothing to do.
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Only top-level sug-*.json — skip the `ned/` subdirectory.
            if path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with("sug-")
                && name.ends_with(".json")
                && let Err(e) = std::fs::remove_file(&path)
            {
                eprintln!(
                    "conductor: failed to delete legacy suggestion {}: {e}",
                    path.display()
                );
            }
        }
    }

    /// Load all NED suggestions from the `.presemble/suggestions/ned/` directory.
    /// Loads all statuses (Pending, Accepted, Rejected, Stale) — callers filter as needed.
    fn load_ned_suggestions(
        dir: &Path,
    ) -> HashMap<editorial_types::SuggestionId, editorial_types::NedSuggestion> {
        let mut map = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "json")
                    && let Ok(contents) = std::fs::read_to_string(entry.path())
                    && let Ok(s) = serde_json::from_str::<editorial_types::NedSuggestion>(&contents)
                {
                    map.insert(s.id.clone(), s);
                }
            }
        }
        map
    }

    /// Persist a NED suggestion to `.presemble/suggestions/ned/<id>.json`.
    fn persist_ned_suggestion(&self, sug: &editorial_types::NedSuggestion) -> std::io::Result<()> {
        let dir = self.ned_suggestions_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", sug.id));
        let json =
            serde_json::to_string_pretty(sug).map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build an ISO-8601 UTC timestamp string (`YYYY-MM-DDTHH:MM:SSZ`).
    fn iso8601_now() -> String {
        use time::format_description::well_known::Iso8601;
        time::OffsetDateTime::now_utc()
            .format(&Iso8601::DEFAULT)
            .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
    }

    /// Derive the grammar for a content file given its site-relative path.
    fn load_grammar_for_file(&self, file: &str) -> Result<schema::Grammar, String> {
        let bpath = std::path::Path::new(file);
        let bcomponents: Vec<_> = bpath.components().collect();
        let stem = if bcomponents.len() == 2 {
            String::new()
        } else {
            bcomponents
                .get(1)
                .and_then(|c| c.as_os_str().to_str())
                .ok_or_else(|| format!("cannot derive schema stem from: {file}"))?
                .to_string()
        };
        let slug = bpath.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let schema_key = site_index::schema_cache_key(&stem, slug);
        let schema_src = self
            .schema_source(&schema_key)
            .ok_or_else(|| format!("slot not in grammar: no schema found for {schema_key}"))?;
        schema::parse_schema(&schema_src).map_err(|e| format!("schema parse error: {e:?}"))
    }

    /// Update build errors and create events from render results.
    fn finalize_render(
        &self,
        rebuilt_pages: Vec<String>,
        failed_pages: Vec<String>,
        new_errors: HashMap<String, Vec<String>>,
        has_stylesheet_change: bool,
    ) -> Vec<ConductorEvent> {
        // Update build errors
        {
            let mut errors = self.build_errors.write().unwrap_or_else(|e| e.into_inner());
            for page in &rebuilt_pages {
                let bare = page.trim_end_matches('/').to_string();
                errors.remove(&bare);
                errors.remove(&format!("{bare}/"));
            }
            for (url, msgs) in new_errors {
                errors.insert(url, msgs);
            }
        }
        // Unsaved edits the repopulation could not keep are build errors too.
        let mut failed_pages = failed_pages;
        for page in self.flush_repopulate_failures() {
            if !failed_pages.contains(&page) {
                failed_pages.push(page);
            }
        }

        let mut events = Vec::new();
        if !rebuilt_pages.is_empty() || has_stylesheet_change {
            events.push(ConductorEvent::PagesRebuilt {
                pages: rebuilt_pages,
                anchor: None,
            });
        }
        if !failed_pages.is_empty() {
            events.push(ConductorEvent::BuildFailed {
                error_pages: failed_pages,
            });
        }
        events
    }

    /// Derive the URL path for a content file (for error tracking).
    fn url_for_content_path(&self, content_path: &Path) -> Option<String> {
        let site_idx = self.site_index.read().unwrap_or_else(|e| e.into_inner());
        let stem = match site_idx.classify(content_path) {
            site_index::FileKind::Content { schema_stem } => schema_stem.as_str().to_string(),
            _ => return None,
        };
        let slug = content_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        Some(site_index::url_for_stem_slug(&stem, slug))
    }

    // ── NED evaluation helpers ─────────────────────────────────────────────────

    /// Build an evaluator root with NED builtins + prelude loaded.
    /// Conductor-specific builtins (query, get-content, etc.) are registered
    /// separately in `editor_server`, a base; components cannot depend on bases,
    /// so those are not available here. Phase C may lift them into a shared
    /// component (`evaluator_bridge`) if MCP needs them.
    ///
    /// The mutating builtins record into `recorder` and write nothing; run the
    /// program with `run_ned_program_with_events`, or evaluate it yourself and
    /// pass `recorder.take()` to [`Conductor::execute_plan`].
    pub(crate) fn make_ned_root(
        &self,
        recorder: &evaluator::ned_primitives::PlanRecorder,
    ) -> Result<evaluator::RootEnv, String> {
        let root = evaluator::RootEnv::new();
        evaluator::init_root(&root).map_err(|e| format!("evaluator init failed: {e}"))?;
        evaluator::ned_primitives::register_ned_builtins(&root, self.node_store(), recorder);
        evaluator::load_ned_prelude(&root).map_err(|e| format!("ned prelude failed: {e}"))?;
        Ok(root)
    }

    /// Build an evaluator root with only the read-only NED builtins and the
    /// prelude (ADR-056). Suggestion selections, at creation and at accept,
    /// are evaluated only in this root, so they can never change the store.
    pub(crate) fn make_ned_query_root(&self) -> Result<evaluator::RootEnv, String> {
        let root = evaluator::RootEnv::new();
        evaluator::init_root(&root).map_err(|e| format!("evaluator init failed: {e}"))?;
        evaluator::ned_primitives::register_ned_query_builtins(&root, self.node_store());
        evaluator::load_ned_prelude(&root).map_err(|e| format!("ned prelude failed: {e}"))?;
        Ok(root)
    }

    /// Evaluate a suggestion selection in the query-only root.
    ///
    /// Must be called without holding any store lock: the query builtins take
    /// their own read locks.
    fn eval_selection(&self, src: &str) -> Result<ned::Selection, editorial_types::StaleCause> {
        let eval_error = |detail: String| editorial_types::StaleCause::EvalError { detail };
        let root = self.make_ned_query_root().map_err(eval_error)?;
        let value = evaluator::eval_str_with_root(src, &root).map_err(eval_error)?;
        evaluator::ned_primitives::extract_selection(&value)
            .map_err(|e| eval_error(format!("the address did not produce a node selection: {e}")))
    }

    /// Mark `paths` dirty and re-render their pages. Returns
    /// `(rebuilt_urls, failed_urls)`. With `blocks_changed` the
    /// pages' references are recomputed first, in place, so a replaced or
    /// removed block is not referenced any more
    /// ([`Conductor::refresh_semantic_content`]), and the pages that show
    /// those pages render again. A page whose references could not be rebuilt
    /// gets a build error and is reported as failed.
    fn mark_dirty_and_rebuild_with(
        &self,
        paths: &[PathBuf],
        blocks_changed: bool,
    ) -> (Vec<String>, Vec<String>) {
        self.mark_unsaved(paths);
        let mut dependents = Vec::new();
        let mut refresh_failed = Vec::new();
        if blocks_changed {
            for path in paths {
                match self.refresh_semantic_content(path) {
                    Ok(urls) => dependents.extend(urls),
                    Err(e) => {
                        let url = self
                            .url_for_content_path(&self.site_dir.join(path))
                            .unwrap_or_else(|| path.display().to_string());
                        eprintln!("conductor: references could not be rebuilt for {url}: {e}");
                        self.build_errors
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .entry(url.clone())
                            .or_default()
                            .push(format!("references could not be rebuilt: {e}"));
                        refresh_failed.push(url);
                    }
                }
            }
        }
        let (mut rebuilt, mut failed) = self.rebuild_pages_for_modified_nodes(paths);
        dependents.sort();
        dependents.dedup();
        for url in dependents {
            match self.render_url(&url) {
                Ok(pages) => rebuilt.extend(pages),
                Err(e) => {
                    eprintln!("conductor: rebuild failed for dependent page {url}: {e}");
                    failed.push(url);
                }
            }
        }
        if blocks_changed {
            rebuilt.sort();
            rebuilt.dedup();
        }
        for url in refresh_failed {
            if !failed.contains(&url) {
                failed.push(url);
            }
        }
        (rebuilt, failed)
    }

    /// Evaluate `selection`, then, under one store write lock, resolve it,
    /// check the file, check whether the change is already in place, check
    /// drift, and run `mutation` as a one-step plan (`edit::execute`). Then
    /// mark the document dirty and re-render its page. The one path both
    /// suggestion accept and a block edit's Apply use.
    ///
    /// `origin` names the caller in plan errors ("accept", "block-edit").
    ///
    /// The target must lie in `file` (the suggestion's file); a selection
    /// that now resolves into another document is `Unresolved`. A `preview`
    /// is checked first for "already in place" (see
    /// [`suggestion_apply::already_in_place`]) and then for drift (see
    /// [`suggestion_apply::check_drift`]). A change that would alter nothing is
    /// [`MutationOutcome::AlreadyInPlace`]: nothing is marked dirty or
    /// rebuilt. Must be called without holding a store lock.
    fn apply_ned_mutation(
        &self,
        selection: &str,
        mutation: &editorial_types::NedMutation,
        file: &Path,
        preview: Option<&editorial_types::SuggestionPreview>,
        origin: &'static str,
    ) -> Result<MutationOutcome, editorial_types::StaleCause> {
        let sel = self.eval_selection(selection)?;
        let path = {
            let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
            let target = edit::resolve(&store, &sel, mutation.target_kind())?;
            // The file check comes first: a block in another document is
            // never "in place" and never drift.
            let document_file = PathBuf::from(
                node_store_bridge::content_bridge::find_attr_text(
                    &store,
                    target.document(),
                    "file",
                )
                .unwrap_or_default(),
            );
            if document_file != file {
                return Err(editorial_types::StaleCause::Unresolved);
            }
            if let Some(preview) = preview {
                if suggestion_apply::already_in_place(&store, &target, mutation, preview) {
                    return Ok(MutationOutcome::AlreadyInPlace);
                }
                suggestion_apply::check_drift(&store, &target, mutation, preview)?;
            }
            let plan: edit::Plan = [edit::PlannedMutation {
                targets: sel,
                mutation: mutation.clone(),
                origin,
            }]
            .into_iter()
            .collect();
            let executed = edit::execute(&mut store, plan)?;
            if !executed.skipped.is_empty() {
                return Ok(MutationOutcome::AlreadyInPlace);
            }
            document_file
        };
        let (pages, failed) =
            self.mark_dirty_and_rebuild_with(&[path], edit::changes_blocks(mutation));
        Ok(MutationOutcome::Changed { pages, failed })
    }

    /// Validate and store a pending NED suggestion (ADR-056): the selection
    /// must address exactly one node of the kind the mutation needs, in
    /// `file`, and the mutation must be valid there. `preview` is display and
    /// drift-check data only.
    fn create_ned_suggestion(
        &self,
        file: &editorial_types::ContentPath,
        selection: String,
        mutation: editorial_types::NedMutation,
        reason: String,
        author: editorial_types::Author,
        preview: Option<editorial_types::SuggestionPreview>,
    ) -> CommandResult {
        let kind = mutation.target_kind();
        let sel = match self.eval_selection(&selection) {
            Ok(sel) => sel,
            Err(cause) => return Self::creation_error(&cause, kind),
        };
        // `(file of the target's document, whether the mutation changes nothing)`.
        let checked: Result<(String, bool), editorial_types::StaleCause> = {
            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
            edit::resolve(&store, &sel, kind)
                .map_err(editorial_types::StaleCause::from)
                .and_then(|target| {
                    let no_change = match edit::prepare(&store, &target, &mutation) {
                        Ok(_) => false,
                        Err(edit::EditError::NoChange) => true,
                        Err(e) => return Err(e.into()),
                    };
                    Ok((
                        node_store_bridge::content_bridge::find_attr_text(
                            &store,
                            target.document(),
                            "file",
                        )
                        .unwrap_or_default(),
                        no_change,
                    ))
                })
        };
        match checked {
            Err(cause) => return Self::creation_error(&cause, kind),
            Ok((doc_file, _)) if Path::new(&doc_file) != Path::new(file.as_str()) => {
                return CommandResult::error(format!(
                    "selection resolves into {doc_file}, not {file}"
                ));
            }
            // The content already reads as suggested: not an error, and
            // nothing to store.
            Ok((_, true)) => return CommandResult::with_response(Response::AlreadyInPlace),
            Ok(_) => {}
        }

        let id = editorial_types::SuggestionId::new();
        let sug = editorial_types::NedSuggestion {
            id: id.clone(),
            author,
            file: file.clone(),
            selection,
            mutation,
            workspace_hash: self.workspace_hash(),
            reason,
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: Self::iso8601_now(),
            preview,
        };

        if let Err(e) = self.persist_ned_suggestion(&sug) {
            return CommandResult::error(format!("persist error: {e}"));
        }
        self.ned_suggestions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), sug.clone());

        CommandResult {
            response: Response::SuggestionCreated(id),
            events: vec![ConductorEvent::NedSuggestionCreated {
                suggestion: Box::new(sug),
            }],
        }
    }

    /// Resolve a block edit's selection to one block in the request's file
    /// and lower the edit. The same for both intents (ADR-059 §10).
    fn lower_block_edit(
        &self,
        req: &editorial_types::BlockEditRequest,
    ) -> Result<
        (node_store_bridge::block_edit::Lowering, Option<usize>),
        editorial_types::BlockEditError,
    > {
        use editorial_types::{BlockEditCause, BlockEditError, TargetKind};
        let refuse = |cause, message: String| BlockEditError { cause, message };
        let sel = self
            .eval_selection(&req.selection)
            .map_err(|c| refuse(BlockEditCause::Unaddressable, c.to_string()))?;
        let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
        let target = match edit::resolve(&store, &sel, TargetKind::Block) {
            Ok(target) => target,
            // The selection names a slot, not a block: an empty slot is filled
            // by a block edit with an empty base (ADR-059 §10).
            Err(edit::AddressError::WrongKind { ref found, .. }) if found == edit::SLOT_KIND => {
                return self
                    .lower_fill_of_slot(&store, &sel, req)
                    .map(|lowering| (lowering, None));
            }
            Err(e) => return Err(refuse(BlockEditCause::Unaddressable, e.to_string())),
        };
        let edit::Target::Block {
            block,
            container,
            document,
        } = target
        else {
            return Err(refuse(
                BlockEditCause::Unaddressable,
                "the selection does not address a block".to_string(),
            ));
        };
        let doc_file = node_store_bridge::content_bridge::find_attr_text(&store, document, "file")
            .unwrap_or_default();
        if Path::new(&doc_file) != Path::new(req.file.as_str()) {
            return Err(refuse(
                BlockEditCause::Unaddressable,
                format!("the selection resolves into {doc_file}, not {}", req.file),
            ));
        }
        // The block's index in `body`, for the scroll anchor (ADR-046).
        let body_idx = match store.get(container) {
            Some(node_store::Node::Element(name))
                if store.resolve_name(*name) == edit::BODY_KIND =>
            {
                store.children(container).iter().position(|c| *c == block)
            }
            _ => None,
        };
        node_store_bridge::block_edit::lower(
            &store,
            block,
            &req.selection,
            &req.base,
            &req.proposed,
        )
        .map(|lowering| (lowering, body_idx))
    }

    /// Lower a block edit whose selection is a slot container: the slot must be
    /// empty, and the proposal is inserted into it
    /// (`block_edit::lower_into_empty_slot`). The kind of block the slot holds,
    /// which decides how the proposal reads (a heading slot's level), comes
    /// from the schema.
    fn lower_fill_of_slot(
        &self,
        store: &node_store::NodeStore,
        sel: &ned::Selection,
        req: &editorial_types::BlockEditRequest,
    ) -> Result<node_store_bridge::block_edit::Lowering, editorial_types::BlockEditError> {
        use editorial_types::{BlockEditCause, BlockEditError, TargetKind};
        let refuse = |cause, message: String| BlockEditError { cause, message };
        let target = edit::resolve(store, sel, TargetKind::Container)
            .map_err(|e| refuse(BlockEditCause::Unaddressable, e.to_string()))?;
        let edit::Target::Container {
            container,
            document,
        } = target
        else {
            return Err(refuse(
                BlockEditCause::Unaddressable,
                "the selection does not address a slot".to_string(),
            ));
        };
        let doc_file = node_store_bridge::content_bridge::find_attr_text(store, document, "file")
            .unwrap_or_default();
        if Path::new(&doc_file) != Path::new(req.file.as_str()) {
            return Err(refuse(
                BlockEditCause::Unaddressable,
                format!("the selection resolves into {doc_file}, not {}", req.file),
            ));
        }
        if !store.children(container).is_empty() {
            return Err(refuse(
                BlockEditCause::Drifted,
                "the slot has blocks now; the edit was made against other content".to_string(),
            ));
        }
        let slot_name = node_store_bridge::content_bridge::find_attr_text(store, container, "name")
            .ok_or_else(|| {
                refuse(
                    BlockEditCause::Unaddressable,
                    "the slot has no name".to_string(),
                )
            })?;
        let grammar = self
            .load_grammar_for_file(req.file.as_str())
            .map_err(|e| refuse(BlockEditCause::Unaddressable, e))?;
        let slot = grammar
            .preamble
            .iter()
            .find(|s| s.name.as_str() == slot_name)
            .ok_or_else(|| {
                refuse(
                    BlockEditCause::Unaddressable,
                    format!("slot '{slot_name}' is not in the schema"),
                )
            })?;
        let like = match &slot.element {
            schema::Element::Heading { level } => content::ContentElement::heading_from_markdown(
                schema::HeadingLevel::new(level.min.value()).unwrap_or(level.min),
                "",
            ),
            _ => content::ContentElement::paragraph_from_markdown(""),
        };
        node_store_bridge::block_edit::lower_into_empty_slot(
            &req.selection,
            &like,
            &req.base,
            &req.proposed,
        )
    }

    /// Handle [`Command::BlockEdit`]: lower, then apply now or store a
    /// suggestion. All decisions are made in `node_store_bridge::block_edit`.
    fn block_edit(&self, req: &editorial_types::BlockEditRequest) -> CommandResult {
        use editorial_types::{BlockEditOutcome, BlockEditResponse};
        match self.lower_block_edit(req) {
            Ok((node_store_bridge::block_edit::Lowering::Lowered(lowered), body_idx)) => {
                self.run_lowered(req.intent, &req.file, lowered, body_idx, req.reason.clone())
            }
            Ok((node_store_bridge::block_edit::Lowering::NoChange, _)) => CommandResult {
                response: Response::BlockEdited(BlockEditResponse::Done(
                    BlockEditOutcome::NoChange,
                )),
                events: vec![],
            },
            Err(e) => CommandResult {
                response: Response::BlockEdited(BlockEditResponse::Refused(e)),
                events: vec![],
            },
        }
    }

    /// The refusal cause for an Apply that could not run. A search that no
    /// longer matches means the block changed since the lowering: drift.
    fn apply_refusal_cause(cause: &editorial_types::StaleCause) -> editorial_types::BlockEditCause {
        use editorial_types::{BlockEditCause, StaleCause};
        match cause {
            StaleCause::Drifted
            | StaleCause::SearchNotFound
            | StaleCause::SearchNotUnique { .. } => BlockEditCause::Drifted,
            StaleCause::Unresolved
            | StaleCause::Ambiguous { .. }
            | StaleCause::WrongKind { .. }
            | StaleCause::EvalError { .. } => BlockEditCause::Unaddressable,
            _ => BlockEditCause::Invalid,
        }
    }

    /// Run a lowered block edit: apply it now or store it as a suggestion.
    ///
    /// Apply passes the preview to the write-locked apply, so the base is
    /// re-checked there: the block cannot change between the read-locked
    /// lowering and the mutation.
    fn run_lowered(
        &self,
        intent: editorial_types::BlockEditIntent,
        file: &editorial_types::ContentPath,
        lowered: node_store_bridge::block_edit::Lowered,
        body_idx: Option<usize>,
        reason: Option<String>,
    ) -> CommandResult {
        use editorial_types::{
            BlockEditCause, BlockEditError, BlockEditIntent, BlockEditOutcome, BlockEditResponse,
        };
        let respond = |response: BlockEditResponse, events: Vec<ConductorEvent>| CommandResult {
            response: Response::BlockEdited(response),
            events,
        };
        let refused = |cause, message: String| {
            respond(
                BlockEditResponse::Refused(BlockEditError { cause, message }),
                vec![],
            )
        };

        match intent {
            BlockEditIntent::Apply => {
                match self.apply_ned_mutation(
                    &lowered.selection,
                    &lowered.mutation,
                    Path::new(file.as_str()),
                    Some(&lowered.preview),
                    "block-edit",
                ) {
                    Ok(MutationOutcome::AlreadyInPlace) => {
                        respond(BlockEditResponse::Done(BlockEditOutcome::NoChange), vec![])
                    }
                    Ok(MutationOutcome::Changed { pages, failed }) => {
                        let mut events = Vec::new();
                        if !pages.is_empty() {
                            events.push(ConductorEvent::PagesRebuilt {
                                pages: pages.clone(),
                                // After the rebuild: the anchor reads the new body.
                                anchor: body_idx
                                    .and_then(|idx| self.compute_body_anchor(file.as_str(), idx)),
                            });
                        }
                        if !failed.is_empty() {
                            events.push(ConductorEvent::BuildFailed {
                                error_pages: failed,
                            });
                        }
                        respond(
                            BlockEditResponse::Done(BlockEditOutcome::Applied { pages }),
                            events,
                        )
                    }
                    Err(cause) => refused(Self::apply_refusal_cause(&cause), cause.to_string()),
                }
            }
            BlockEditIntent::Suggest => {
                let created = self.create_ned_suggestion(
                    file,
                    lowered.selection,
                    lowered.mutation,
                    reason.unwrap_or_default(),
                    // TODO: carry session/user id when multiplayer suggestions land
                    editorial_types::Author::Human("browser".to_string()),
                    Some(lowered.preview),
                );
                match created.response {
                    Response::SuggestionCreated(id) => respond(
                        BlockEditResponse::Done(BlockEditOutcome::SuggestionCreated { id }),
                        created.events,
                    ),
                    Response::AlreadyInPlace => {
                        respond(BlockEditResponse::Done(BlockEditOutcome::NoChange), vec![])
                    }
                    Response::Error(message) => refused(BlockEditCause::Invalid, message),
                    other => refused(
                        BlockEditCause::Invalid,
                        format!("unexpected conductor response: {other:?}"),
                    ),
                }
            }
        }
    }

    /// Format a creation-time rejection: the cause as a sentence, plus a hint
    /// when one applies.
    fn creation_error(
        cause: &editorial_types::StaleCause,
        kind: editorial_types::TargetKind,
    ) -> CommandResult {
        match suggestion_apply::creation_hint(cause, kind) {
            Some(hint) => CommandResult::error(format!("{cause}. Hint: {hint}")),
            None => CommandResult::error(format!("{cause}")),
        }
    }

    /// Run a NED program as a plan (select-then-apply, ADR-059 stage 2e), for
    /// `/apply` behind `handle_command`.
    ///
    /// `root` is a root built with [`Conductor::make_ned_root`] around
    /// `recorder`. The program is evaluated with no store lock held: its
    /// queries see the store as it is now, and its mutating builtins only
    /// record. The recorded plan then goes to [`Conductor::execute_plan`].
    ///
    /// Returns the program's value, what changed, and the events to publish.
    /// Why it did not run (nothing is applied) is a [`ProgramError`]: an
    /// evaluation error, or the structured refusal of one step. Its `Display`
    /// is `eval error: ...` or the step's sentence, e.g.
    /// `step 2 (ned/set-text): ...`.
    fn run_ned_program_with_events(
        &self,
        root: &evaluator::RootEnv,
        recorder: &evaluator::ned_primitives::PlanRecorder,
        code: &str,
    ) -> Result<(template::Value, ProgramOutcome, Vec<ConductorEvent>), ProgramError> {
        // The primitives take their own read locks: hold none here.
        let value = evaluator::eval_str_with_root(code, root).map_err(ProgramError::Eval)?;

        let (outcome, events) = self.execute_plan(recorder.take())?;
        Ok((value, outcome, events))
    }

    /// Run a recorded plan: all of it, or nothing (`content_store::edit::execute`
    /// under one write lock), then mark the changed documents dirty and
    /// re-render their pages.
    ///
    /// The single writer entry point for plans from outside the conductor: the
    /// daemon's command loop calls it for the nREPL's plans, and `/apply`
    /// reaches it through `handle_command`. Nothing else may write to the
    /// store from another thread.
    ///
    /// Returns what changed and the events to publish for it
    /// (`PagesRebuilt`, `BuildFailed`); the caller owns publishing them. The
    /// daemon's command loop is the one place that calls this for the nREPL,
    /// so every writer runs on one thread. An empty plan changes nothing and
    /// has no events.
    pub fn execute_plan(
        &self,
        plan: edit::Plan,
    ) -> Result<(ProgramOutcome, Vec<ConductorEvent>), ProgramError> {
        if plan.is_empty() {
            return Ok((ProgramOutcome::default(), vec![]));
        }
        let steps = plan.len();
        let blocks_changed = plan.changes_blocks();

        let (dirty_paths, skipped) = {
            let mut store = self.node_store.write().unwrap_or_else(|e| e.into_inner());
            let executed = edit::execute(&mut store, plan)
                .map_err(|e| ProgramError::Plan(editorial_types::PlanFailure::from(e)))?;
            // Documents without a file (synthetic roots) have nothing to save.
            let paths: Vec<PathBuf> = executed
                .documents
                .iter()
                .filter_map(|doc| {
                    node_store_bridge::content_bridge::find_attr_text(&store, *doc, "file")
                })
                .filter(|file| !file.is_empty())
                .map(PathBuf::from)
                .collect();
            (paths, executed.skipped.len())
        };

        let (rebuilt_pages, failed_pages) = if dirty_paths.is_empty() {
            (vec![], vec![])
        } else {
            self.mark_dirty_and_rebuild_with(&dirty_paths, blocks_changed)
        };
        let mut events = Vec::new();
        if !rebuilt_pages.is_empty() {
            events.push(ConductorEvent::PagesRebuilt {
                pages: rebuilt_pages.clone(),
                anchor: None,
            });
        }
        if !failed_pages.is_empty() {
            events.push(ConductorEvent::BuildFailed {
                error_pages: failed_pages.clone(),
            });
        }
        Ok((
            ProgramOutcome {
                dirty_paths,
                rebuilt_pages,
                failed_pages,
                applied: steps - skipped,
                skipped,
            },
            events,
        ))
    }

    /// Run a NED program for `/apply` (see `run_ned_program_with_events`).
    ///
    /// The response shape is unchanged: `Response::Applied` with the number of
    /// dirty documents, or `Response::Error` with why the program was
    /// refused.
    fn apply_ned_program(&self, program: &str) -> CommandResult {
        // Build the evaluator root — does not hold any store lock.
        let recorder = evaluator::ned_primitives::PlanRecorder::new();
        let root = match self.make_ned_root(&recorder) {
            Ok(r) => r,
            Err(e) => return CommandResult::error(format!("evaluator init: {e}")),
        };
        let (outcome, events) = match self.run_ned_program_with_events(&root, &recorder, program) {
            Ok((_value, outcome, events)) => (outcome, events),
            Err(ProgramError::Plan(failure)) => {
                return CommandResult::with_response(Response::PlanRefused(failure));
            }
            Err(e @ ProgramError::Eval(_)) => return CommandResult::error(e.to_string()),
        };

        let ProgramOutcome {
            dirty_paths,
            rebuilt_pages: rebuilt_urls,
            failed_pages: failed_urls,
            ..
        } = outcome;
        let dirty_path_count = dirty_paths.len();

        CommandResult {
            response: Response::Applied {
                rebuilt_pages: rebuilt_urls,
                failed_pages: failed_urls,
                dirty_paths: dirty_path_count,
            },
            events,
        }
    }

    fn stem_from_url_path(url: &str) -> String {
        let trimmed = url.trim_start_matches('/');
        if trimmed.is_empty() {
            return String::new();
        }
        trimmed.split('/').next().unwrap_or("").to_string()
    }

    fn render_page(&self, url_path: &str, mode: &RenderMode) -> Result<String, String> {
        match mode {
            RenderMode::View => {
                // A URL segment becomes a directory name below the output
                // directory: only plain names, and the file must stay inside.
                editorial_types::plain_url_segments(url_path)
                    .map_err(|e| format!("not a page URL: {url_path}: {e}"))?;
                let stem = Self::stem_from_url_path(url_path);
                let slug = if url_path.ends_with('/') || url_path == "/" {
                    "index".to_string()
                } else {
                    url_path.rsplit('/').next().unwrap_or("index").to_string()
                };
                let out = site_index::output_path_for_stem_slug(&self.output_dir, &stem, &slug);
                editorial_types::ensure_within(&self.output_dir, &out)
                    .map_err(|e| format!("not a page URL: {url_path}: {e}"))?;
                std::fs::read_to_string(&out).map_err(|e| format!("no output for {url_path}: {e}"))
            }
            RenderMode::Schema => {
                let (stem, kind) = site_index::parse_schema_url(url_path)
                    .ok_or_else(|| format!("not a schema URL: {url_path}"))?;
                if !self.is_known_stem(&stem) {
                    return Err(format!("unknown schema stem: {stem}"));
                }
                let grammar_key = match kind {
                    schema::SchemaKind::Index => site_index::schema_cache_key(&stem, "index"),
                    schema::SchemaKind::Item => {
                        let item_key = format!("{stem}/item");
                        let c = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
                        if c.contains_key(&item_key) {
                            item_key
                        } else {
                            stem.clone()
                        }
                    }
                };
                let grammar_src = {
                    let c = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
                    c.get(&grammar_key).cloned()
                }
                .ok_or_else(|| format!("no schema for {url_path} (key={grammar_key})"))?;
                let grammar =
                    schema::parse_schema(&grammar_src).map_err(|e| format!("schema parse: {e}"))?;
                let doc = node_store_bridge::synthesize_schema_document(&grammar, kind);
                let article_graph =
                    template::build_article_graph(&doc, &grammar, template::OutputMode::Editorial);
                let mut ctx_graph = template::DataGraph::new();
                ctx_graph.insert("_presemble_stem", template::Value::Text(stem.clone()));
                ctx_graph.insert("_presemble_file", template::Value::Text(String::new()));
                ctx_graph.insert("url", template::Value::Text(url_path.to_string()));
                ctx_graph.insert("input", template::Value::Record(article_graph));
                let fresh_repo = site_repository::SiteRepository::builder()
                    .from_dir(&self.site_dir)
                    .build();
                let stem_obj = site_index::SchemaStem::new(&stem);
                let tmpl = if matches!(kind, schema::SchemaKind::Index) {
                    fresh_repo
                        .collection_template_source(&stem_obj)
                        .or_else(|| fresh_repo.item_template_source(&stem_obj))
                        .or_else(|| fresh_repo.partial_template_source(&stem))
                } else {
                    fresh_repo
                        .item_template_source(&stem_obj)
                        .or_else(|| fresh_repo.partial_template_source(&stem))
                }
                .ok_or_else(|| format!("no template for stem {stem}"))?;
                let (tmpl_src, is_hiccup) = tmpl;
                let raw = if is_hiccup {
                    template::parse_template_hiccup(&tmpl_src).map_err(|e| format!("{e}"))?
                } else {
                    template::parse_template_xml(&tmpl_src).map_err(|e| format!("{e}"))?
                };
                let reg = template_registry::FileTemplateRegistry::new(fresh_repo);
                let (nodes, local_defs) = template::extract_definitions(raw);
                let render_ctx = template::RenderContext::with_local_defs(
                    &reg,
                    &local_defs,
                    template::OutputMode::Editorial,
                );
                let transformed = template::transform(nodes, &ctx_graph, &render_ctx)
                    .map_err(|e| format!("render: {e}"))?;

                // T6: attach instance-count + sample-URL attrs to the first root element.
                let count = self
                    .site_index
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .content_files(&stem)
                    .len();
                let sample_url = self
                    .site_index
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .sample_url_for_stem(&stem);
                let transformed = template::attach_schema_instance_attrs(
                    transformed,
                    count,
                    sample_url.as_deref(),
                );

                // T7: attach included-by back-reference JSON attr.
                let including_stems = self
                    .site_index
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .schemas_including(&stem);
                let included_pairs_owned: Vec<(String, String)> = including_stems
                    .iter()
                    .map(|s| (s.clone(), format!("/{s}/#_schema")))
                    .collect();
                let included_pairs: Vec<(&str, &str)> = included_pairs_owned
                    .iter()
                    .map(|(s, u)| (s.as_str(), u.as_str()))
                    .collect();
                let transformed =
                    template::attach_schema_included_by_attr(transformed, &included_pairs);

                Ok(template::serialize_nodes(&transformed))
            }
        }
    }

    /// Handle a command and return a response plus any events to broadcast.
    pub fn handle_command(&self, cmd: Command) -> CommandResult {
        match cmd {
            Command::Ping => CommandResult::with_response(Response::Pong),
            Command::GetGrammar { stem } => {
                CommandResult::with_response(Response::SchemaSource(self.schema_source(&stem)))
            }
            Command::GetDocumentText { path } => {
                CommandResult::with_response(Response::DocumentText(self.document_text(&path)))
            }
            Command::GetBuildErrors => {
                let errors = self.build_errors.read().unwrap_or_else(|e| e.into_inner());
                CommandResult::with_response(Response::BuildErrors(errors.clone()))
            }
            Command::Shutdown => CommandResult::ok(),
            Command::EditorOpened { path, text } => self.editor_opened(path, text),
            Command::EditorChanged { path, text } => self.editor_changed(path, text),
            Command::EditorClosed { path } => self.editor_closed(&path),
            Command::KeepMine { path, drift_id } => self.keep_mine(path, drift_id),
            Command::DocumentSaved { path } => {
                if let Err(e) = crate::site_resolve::resolve_in_site(&self.site_dir, &path) {
                    return CommandResult::error(format!("{path}: {e}"));
                }
                // The editor wrote the file. If it holds the editor's latest
                // text, or reads as what N holds, that is a save (or a flush
                // of N); otherwise, if the store holds unsaved edits for it,
                // they now conflict with what was written (ADR-065).
                if let Some(events) = self.settle_disk_write(path.as_path()) {
                    return CommandResult::ok_with_events(events);
                }
                match self.take_disk_change(path.as_path()) {
                    Some(events) if !events.is_empty() => CommandResult::ok_with_events(events),
                    _ => CommandResult::ok(),
                }
            }
            Command::FileChanged { paths } => {
                // 0. Filter out paths that the conductor wrote itself.
                //    We resolve each path to an absolute path and check the on-disk mtime.
                //    If the mtime matches a recorded self-write, the event is suppressed.
                //    Deletions (no metadata) are always processed.
                //    A path that resolves outside the site is not ours: it is dropped.
                let external_paths: Vec<editorial_types::SitePath> = paths
                    .iter()
                    .filter(|p| {
                        let Ok(abs) = crate::site_resolve::resolve_in_site(&self.site_dir, p)
                        else {
                            return false;
                        };
                        let mtime = std::fs::metadata(&abs).ok().and_then(|m| m.modified().ok());
                        match mtime {
                            Some(mtime) => !self.self_write_tracker.should_ignore(&abs, mtime),
                            None => true, // deletion — always process
                        }
                    })
                    .cloned()
                    .collect();

                // If every changed path was written by us, skip the rebuild entirely.
                if external_paths.is_empty() {
                    return CommandResult::ok();
                }

                // Rebind so the rest of this handler uses the filtered set.
                let paths = external_paths;

                // 2. Refresh site index for new/removed files
                {
                    let mut idx = self.site_index.write().unwrap_or_else(|e| e.into_inner());
                    *idx = site_index::SiteIndex::new(self.site_dir.clone());
                }

                // 3. Refresh schema cache for changed schemas
                self.refresh_schema_cache();

                // 4. Repopulate the node store.
                // A full re-render (D9). The snapshot comparison CreateContent
                // uses (`snapshot_query_results` before and after the
                // repopulate) is the basis for a minimal FileChanged later.
                // A changed file with unsaved edits is a conflict (ADR-065):
                // its edits are kept and the disk version does not win. Every
                // other changed file takes the disk version.
                let mut disk_wins: Vec<PathBuf> = Vec::new();
                let mut conflict_events: Vec<ConductorEvent> = Vec::new();
                for p in &paths {
                    let rel = p.as_path().to_path_buf();
                    if let Some(events) = self.settle_disk_write(&rel) {
                        // The editor's own write, or a file that reads as N:
                        // N already holds what the file does (or was written
                        // over it), so the disk version is not loaded.
                        conflict_events.extend(events);
                    } else if let Some(events) = self.take_disk_change(&rel) {
                        // Unsaved edits: the change is merged into them, or
                        // conflicts (ADR-065).
                        conflict_events.extend(events);
                    } else {
                        disk_wins.push(rel);
                    }
                }
                self.populate_node_store(&disk_wins);

                // 5. Detect stylesheet changes (for reload signalling)
                let has_stylesheet_change = {
                    let site_idx = self.site_index.read().unwrap_or_else(|e| e.into_inner());
                    paths.iter().any(|p| {
                        let path = self.site_dir.join(p.as_path());
                        matches!(site_idx.classify(&path), site_index::FileKind::Stylesheet)
                    })
                };

                // 6. Batch-render all pages using O(n) pipeline
                let (rebuilt_pages, failed_pages, new_errors) =
                    self.build_all_pages(template::OutputMode::Editorial);

                // 7-8. Update build errors and build events
                let mut events = self.finalize_render(
                    rebuilt_pages,
                    failed_pages,
                    new_errors,
                    has_stylesheet_change,
                );
                events.extend(conflict_events);

                if events.is_empty() {
                    CommandResult::ok()
                } else {
                    CommandResult::ok_with_events(events)
                }
            }
            Command::ReloadDocument { path } => {
                // Throw away this document's unsaved edits and load it from
                // disk (an editor attached is given the disk text). Every other document
                // keeps its unsaved edits (the repopulate rule).
                if let Err(e) =
                    crate::site_resolve::resolve_in_site(&self.site_dir, path.as_site_path())
                {
                    return CommandResult::error(format!("{path}: {e}"));
                }
                let rel = PathBuf::from(path.as_str());
                let was_dirty = self
                    .dirty_docs
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&rel);
                self.clear_unsaved(&rel);
                self.populate_node_store(&[rel]);
                // An editor of it is given what N has now.
                self.editor_follows_reload(&path);

                // The store changed under every page: re-render, as after a
                // file change.
                let (rebuilt_pages, failed_pages, new_errors) =
                    self.build_all_pages(template::OutputMode::Editorial);
                let events = self.finalize_render(rebuilt_pages, failed_pages, new_errors, false);
                CommandResult {
                    response: Response::Reloaded {
                        discarded: was_dirty,
                    },
                    events,
                }
            }
            Command::CursorMoved { path, line } => {
                // An open editor's text is the one the line is in.
                if let Some(src) = self
                    .editor_text(&path)
                    .or_else(|| self.document_text(&path))
                    && let Some(anchor) =
                        self.body_element_anchor_at_line(&src, path.as_str(), line)
                {
                    return CommandResult::ok_with_events(vec![ConductorEvent::CursorScrollTo {
                        anchor,
                    }]);
                }
                CommandResult::ok()
            }
            Command::ApplyNedProgram { program } => self.apply_ned_program(&program),
            Command::CreateContent { stem, slug } => {
                // Use a fresh repo to find current schemas (self.repo may be stale after scaffold)
                let fresh_repo = site_repository::SiteRepository::builder()
                    .from_dir(&self.site_dir)
                    .build();
                match content_editor::create_content(&self.site_dir, &fresh_repo, &stem, &slug) {
                    Ok((path, url)) => {
                        // ADR-043: the new file is our own write, so the
                        // watcher's event for it must not look external.
                        // (`create_content` also creates `content/<stem>/` if
                        // needed; the tracker follows files, not directories.)
                        if let Ok(mtime) = std::fs::metadata(&path).and_then(|m| m.modified()) {
                            self.self_write_tracker.record(path.clone(), mtime);
                        }
                        // Link-expression results before the repopulate, to
                        // find the pages the new document changes.
                        let results_before = self.snapshot_query_results();

                        // Refresh schema cache, site index, and the node store for the new content
                        self.refresh_schema_cache();
                        self.refresh_site_index();
                        // The new file is the one the disk wins for; unsaved
                        // edits in every other document are kept.
                        let created = path
                            .strip_prefix(&self.site_dir)
                            .unwrap_or(&path)
                            .to_path_buf();
                        self.populate_node_store(&[created]);

                        let mut rebuilt_pages: Vec<String> = vec![];

                        // Render the pages whose link-expression results
                        // changed (the new page itself is rendered below).
                        let results_after = self.snapshot_query_results();
                        let changed_pages: std::collections::BTreeSet<String> = results_after
                            .iter()
                            .filter(|(key, urls)| results_before.get(*key) != Some(*urls))
                            .map(|((page, _), _)| page.clone())
                            .chain(
                                results_before
                                    .keys()
                                    .filter(|key| !results_after.contains_key(*key))
                                    .map(|(page, _)| page.clone()),
                            )
                            .filter(|page| *page != url)
                            .collect();
                        for page in changed_pages {
                            match self.render_url(&page) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!("conductor: rebuild failed for {page}: {e}"),
                            }
                        }

                        // Rebuild the new content page itself
                        if let Some(text) = self.document_text_at(&path) {
                            match self.rebuild_page(&path, &text) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!(
                                    "conductor: rebuild failed for new content {}: {e}",
                                    path.display()
                                ),
                            }
                        }

                        // Rebuild the collection index page if it exists
                        let collection_index =
                            self.site_dir.join("content").join(&stem).join("index.md");
                        if collection_index.exists()
                            && let Some(text) = self.document_text_at(&collection_index)
                        {
                            match self.rebuild_page(&collection_index, &text) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!(
                                    "conductor: rebuild failed for collection index {}: {e}",
                                    collection_index.display()
                                ),
                            }
                        }

                        // Rebuild the site root index if it exists
                        let site_index_path = self.site_dir.join("content").join("index.md");
                        if site_index_path.exists()
                            && let Some(text) = self.document_text_at(&site_index_path)
                        {
                            match self.rebuild_page(&site_index_path, &text) {
                                Ok(mut pages) => rebuilt_pages.append(&mut pages),
                                Err(e) => eprintln!(
                                    "conductor: rebuild failed for site index {}: {e}",
                                    site_index_path.display()
                                ),
                            }
                        }

                        let mut events = Vec::new();
                        if !rebuilt_pages.is_empty() {
                            events.push(ConductorEvent::PagesRebuilt {
                                pages: rebuilt_pages,
                                anchor: None,
                            });
                        }
                        let failed = self.flush_repopulate_failures();
                        if !failed.is_empty() {
                            events.push(ConductorEvent::BuildFailed {
                                error_pages: failed,
                            });
                        }
                        CommandResult {
                            response: Response::ContentCreated(url),
                            events,
                        }
                    }
                    Err(e) => CommandResult::error(e),
                }
            }
            Command::GetDirtyBuffers => {
                // The documents N holds unsaved edits for, site-relative
                // (e.g. "content/post/first.md"). An editor's text is not a
                // buffer of its own: what it types is in N.
                let paths: std::collections::BTreeSet<String> = self
                    .dirty_docs
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();

                CommandResult::with_response(Response::DirtyBuffers(paths.into_iter().collect()))
            }
            Command::GetConflicts => {
                let dirty = self.dirty_docs.read().unwrap_or_else(|e| e.into_inner());
                let mut files: Vec<crate::protocol::FileConflicts> = dirty
                    .conflicts()
                    .map(|p| crate::protocol::FileConflicts {
                        file: p.to_string_lossy().to_string(),
                        regions: dirty.conflict_regions(p).unwrap_or_default().to_vec(),
                    })
                    .collect();
                files.sort_by(|a, b| a.file.cmp(&b.file));
                CommandResult::with_response(Response::Conflicts(files))
            }
            Command::SaveBuffer { path, consent } => {
                if let Err(e) = crate::site_resolve::resolve_in_site(&self.site_dir, &path) {
                    return CommandResult::error(format!("{path}: {e}"));
                }
                let rel_path = path.as_path().to_path_buf();

                // The watcher may not have reported a change to the file yet:
                // look at the file ourselves first (ADR-065).
                let detected = self.detect_unreported_change(&rel_path);

                // A save never overwrites a conflict without consent that
                // names the file (ADR-065). Nothing is written.
                if self
                    .dirty_docs
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_conflicted(&rel_path)
                    && !consent.covers(path.as_str())
                {
                    return CommandResult {
                        response: Response::ConflictsNeedConfirmation(vec![path.to_string()]),
                        events: detected.into_iter().collect(),
                    };
                }

                // Serialize from NodeStore and write.
                if self
                    .dirty_docs
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&rel_path)
                {
                    let text = match self.doc_root_for_path(&rel_path) {
                        Some(root) => {
                            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                            match node_store_bridge::serialize_from_store(&store, root) {
                                Ok(text) => text,
                                Err(e) => {
                                    return CommandResult::error(format!(
                                        "cannot save {path}: {e}"
                                    ));
                                }
                            }
                        }
                        None => {
                            return CommandResult::error(format!("document not in store: {path}"));
                        }
                    };
                    if let Err(e) = self.write_document(&rel_path, text.as_bytes()) {
                        return CommandResult::error(format!("write error: {e}"));
                    }
                    self.clear_unsaved(&rel_path);
                    return CommandResult::ok_with_events(vec![saved_event([&rel_path])]);
                }
                CommandResult::error(format!("buffer not dirty: {path}"))
            }
            Command::SaveAllBuffers { consent } => {
                // The check comes first (ADR-065): if any document we would
                // write is in conflict with the file on disk and the consent
                // does not name it, write nothing and say which.
                //
                // The watcher may not have reported every change yet, so look
                // at each dirty document's file first.
                let dirty_paths: Vec<PathBuf> = self
                    .dirty_docs
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .iter()
                    .map(Path::to_path_buf)
                    .collect();
                let detected: Vec<ConductorEvent> = dirty_paths
                    .iter()
                    .flat_map(|rel| self.detect_unreported_change(rel))
                    .collect();
                let uncovered: Vec<String> = {
                    let dirty = self.dirty_docs.read().unwrap_or_else(|e| e.into_inner());
                    let mut files: Vec<String> = dirty
                        .conflicts()
                        .map(|p| p.to_string_lossy().to_string())
                        .filter(|file| !consent.covers(file))
                        .collect();
                    files.sort();
                    files
                };
                if !uncovered.is_empty() {
                    return CommandResult {
                        response: Response::ConflictsNeedConfirmation(uncovered),
                        events: detected,
                    };
                }

                // Save NED-dirty documents (NodeStore → disk)
                let ned_docs = self
                    .dirty_docs
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                // Documents that fail to decode or write stay dirty (and keep
                // their conflict) and are reported after the remaining
                // buffers have been saved.
                let mut failures: Vec<String> = Vec::new();
                // What was written to disk, for `DocumentsSaved`.
                let mut saved: Vec<PathBuf> = Vec::new();
                for (rel_path, state) in &ned_docs {
                    let keep_dirty = |dirty: &RwLock<crate::dirty::DirtyDocs>| {
                        dirty
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .restore(rel_path.clone(), state.clone());
                    };
                    let text = match self.doc_root_for_path(rel_path) {
                        Some(root) => {
                            let store = self.node_store.read().unwrap_or_else(|e| e.into_inner());
                            match node_store_bridge::serialize_from_store(&store, root) {
                                Ok(text) => text,
                                Err(e) => {
                                    drop(store);
                                    keep_dirty(&self.dirty_docs);
                                    failures
                                        .push(format!("cannot save {}: {e}", rel_path.display()));
                                    continue;
                                }
                            }
                        }
                        None => {
                            eprintln!(
                                "conductor: SaveAllBuffers: no store root for {}",
                                rel_path.display()
                            );
                            keep_dirty(&self.dirty_docs);
                            failures.push(format!(
                                "cannot save {}: document not in store",
                                rel_path.display()
                            ));
                            continue;
                        }
                    };
                    if let Err(e) = self.write_document(rel_path, text.as_bytes()) {
                        keep_dirty(&self.dirty_docs);
                        failures.push(format!("write error for {}: {e}", rel_path.display()));
                        continue;
                    }
                    // Saved: its edits have no base any more. (One kept dirty
                    // above keeps its base.)
                    self.disk_bases
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear(rel_path);
                    saved.push(rel_path.clone());
                }

                if !failures.is_empty() {
                    return CommandResult::error(failures.join("; "));
                }
                if saved.is_empty() {
                    return CommandResult::ok();
                }
                CommandResult::ok_with_events(vec![saved_event(saved.iter())])
            }
            Command::GetNedSuggestionFiles => {
                let suggestions = self
                    .ned_suggestions
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let files: Vec<String> = suggestions
                    .values()
                    .filter(|s| {
                        matches!(
                            s.status,
                            editorial_types::NedSuggestionStatus::Pending
                                | editorial_types::NedSuggestionStatus::Stale { .. }
                        )
                    })
                    .map(|s| s.file.to_string())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                CommandResult::with_response(Response::NedSuggestionFiles(files))
            }
            Command::ScaffoldSite {
                template_name,
                format,
                font_mood,
                seed_color,
                palette_type,
                complexity,
                theme,
            } => {
                match site_templates::template_by_name(&template_name) {
                    Some(template) => {
                        let style = site_templates::StyleConfig {
                            font_mood: font_mood.parse().unwrap_or_default(),
                            seed_color: if seed_color.is_empty() {
                                site_templates::StyleConfig::default().seed_color
                            } else {
                                seed_color
                            },
                            palette_type: palette_type.parse().unwrap_or_default(),
                            complexity: complexity.parse().unwrap_or_default(),
                            theme: theme.parse().unwrap_or_default(),
                        };
                        match template.scaffold(&self.site_dir, &format, &style) {
                            Ok(()) => {
                                // Record all written source files so the watcher suppresses the
                                // resulting events (they would otherwise trigger a redundant
                                // populate_node_store / build_all_pages cycle).
                                for subdir in &["schemas", "content", "templates"] {
                                    crate::self_write_tracker::record_dir_recursive(
                                        &self.site_dir.join(subdir),
                                        &self.self_write_tracker,
                                    );
                                }
                                // Refresh schema cache and site index — new schemas/dirs were written to disk
                                self.refresh_schema_cache();
                                self.refresh_site_index();
                                // Repopulate the node store with the new content
                                self.populate_node_store(&[]);

                                // Batch-render all pages using O(n) pipeline
                                let (rebuilt, failed, errors) =
                                    self.build_all_pages(template::OutputMode::Editorial);
                                let events = self.finalize_render(rebuilt, failed, errors, false);

                                if events.is_empty() {
                                    CommandResult::ok()
                                } else {
                                    CommandResult::ok_with_events(events)
                                }
                            }
                            Err(e) => CommandResult::error(e),
                        }
                    }
                    None => CommandResult::error(format!("unknown template: {template_name}")),
                }
            }
            Command::Classify { path } => {
                let Ok(abs_path) = crate::site_resolve::resolve_in_site(&self.site_dir, &path)
                else {
                    // Outside the site: not a site file.
                    return CommandResult::with_response(Response::FileClassification(
                        FileClassification::Unknown,
                    ));
                };
                let kind = self
                    .site_index
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .classify(&abs_path);
                let classification = match kind {
                    site_index::FileKind::Content { schema_stem } => FileClassification::Content {
                        schema_stem: schema_stem.to_string(),
                    },
                    site_index::FileKind::Template { schema_stem } => {
                        FileClassification::Template {
                            schema_stem: schema_stem.to_string(),
                        }
                    }
                    site_index::FileKind::Schema { stem } => FileClassification::Schema {
                        stem: stem.to_string(),
                    },
                    site_index::FileKind::Stylesheet => FileClassification::Stylesheet,
                    site_index::FileKind::Asset => FileClassification::Asset,
                    site_index::FileKind::Unknown => FileClassification::Unknown,
                };
                CommandResult::with_response(Response::FileClassification(classification))
            }
            Command::ListSchemas => {
                let cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
                let result: Vec<(String, String)> = cache
                    .iter()
                    .map(|(stem, src)| (stem.clone(), src.clone()))
                    .collect();
                CommandResult::with_response(Response::SchemaList(result))
            }
            Command::ListLinkOptions { stem } => {
                if !self.is_known_stem(&stem) {
                    return CommandResult::error(format!("unknown schema stem: {stem}"));
                }
                let options = self.list_link_options(&stem);
                CommandResult::with_response(Response::LinkOptions(options))
            }
            Command::ResolveLinkTargetStem { source_stem, slot } => {
                let target = self.resolve_link_target_stem(&source_stem, &slot);
                CommandResult::with_response(Response::LinkTargetStem(target))
            }
            Command::ListTemplateNames => CommandResult::with_response(Response::TemplateNames(
                template_names(&self.site_dir),
            )),
            Command::ListDependents { stem } => {
                let site_files = self
                    .site_index
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .dependents_of_schema(&stem);
                let dependents: Vec<DependentFile> = site_files
                    .into_iter()
                    .map(|sf| {
                        let path = sf.path.to_string_lossy().to_string();
                        let kind = match sf.kind {
                            site_index::FileKind::Content { schema_stem } => {
                                FileClassification::Content {
                                    schema_stem: schema_stem.to_string(),
                                }
                            }
                            site_index::FileKind::Template { schema_stem } => {
                                FileClassification::Template {
                                    schema_stem: schema_stem.to_string(),
                                }
                            }
                            site_index::FileKind::Schema { stem: s } => {
                                FileClassification::Schema {
                                    stem: s.to_string(),
                                }
                            }
                            site_index::FileKind::Stylesheet => FileClassification::Stylesheet,
                            site_index::FileKind::Asset => FileClassification::Asset,
                            site_index::FileKind::Unknown => FileClassification::Unknown,
                        };
                        DependentFile { path, kind }
                    })
                    .collect();
                CommandResult::with_response(Response::Dependents(dependents))
            }
            Command::ListContent => {
                let site_index = self.site_index.read().unwrap_or_else(|e| e.into_inner());
                let stems = site_index.schema_stems();
                let mut paths = Vec::new();
                for stem in &stems {
                    for file_path in site_index.content_files(stem) {
                        let rel = file_path.strip_prefix(&self.site_dir).unwrap_or(&file_path);
                        paths.push(rel.to_string_lossy().to_string());
                    }
                }
                for file_path in site_index.content_files("") {
                    let rel = file_path.strip_prefix(&self.site_dir).unwrap_or(&file_path);
                    paths.push(rel.to_string_lossy().to_string());
                }
                drop(site_index);
                paths.sort();
                paths.dedup();
                CommandResult::with_response(Response::ContentList(paths))
            }

            // ── NED suggestion commands ───────────────────────────────────────
            Command::BlockEdit(req) => self.block_edit(&req),
            Command::CreateNedSuggestion {
                file,
                selection,
                mutation,
                reason,
                author,
            } => self.create_ned_suggestion(&file, selection, mutation, reason, author, None),

            Command::AcceptNedSuggestion { id } => {
                // 1. Look up suggestion; must be Pending
                let sug = {
                    let map = self
                        .ned_suggestions
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    match map.get(&id) {
                        Some(s) if s.status == editorial_types::NedSuggestionStatus::Pending => {
                            s.clone()
                        }
                        Some(_) => {
                            return CommandResult::error(format!(
                                "ned suggestion {id} is not pending"
                            ));
                        }
                        None => {
                            return CommandResult::error(format!("ned suggestion not found: {id}"));
                        }
                    }
                };

                // Helper: mark Stale, persist, emit event, return Ok
                let mark_stale = |conductor: &Conductor,
                                  mut s: editorial_types::NedSuggestion,
                                  cause: editorial_types::StaleCause|
                 -> CommandResult {
                    let reason = cause.to_string();
                    s.status = editorial_types::NedSuggestionStatus::Stale {
                        reason: reason.clone(),
                        cause,
                    };
                    if let Err(e) = conductor.persist_ned_suggestion(&s) {
                        eprintln!("conductor: failed to persist stale ned suggestion: {e}");
                    }
                    conductor
                        .ned_suggestions
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(s.id.clone(), s.clone());
                    CommandResult::ok_with_events(vec![ConductorEvent::NedSuggestionStaled {
                        id: s.id,
                        file: s.file.clone(),
                        reason,
                    }])
                };

                // 2-4. Evaluate, resolve, check drift, validate and apply as
                // a one-step plan under one store write lock (ADR-056), then
                // mark the document dirty and re-render its page. A change
                // that is already in place counts as applied: nothing is
                // written, marked dirty or rebuilt.
                let (pages, failed) = match self.apply_ned_mutation(
                    &sug.selection,
                    &sug.mutation,
                    Path::new(sug.file.as_str()),
                    sug.preview.as_ref(),
                    "accept",
                ) {
                    Ok(MutationOutcome::Changed { pages, failed }) => (pages, failed),
                    Ok(MutationOutcome::AlreadyInPlace) => (Vec::new(), Vec::new()),
                    Err(cause) => return mark_stale(self, sug, cause),
                };

                // 5. Success: mark Accepted
                let mut updated = sug.clone();
                updated.status = editorial_types::NedSuggestionStatus::Accepted;
                if let Err(e) = self.persist_ned_suggestion(&updated) {
                    eprintln!("conductor: failed to persist accepted ned suggestion: {e}");
                }
                let accepted_file = updated.file.clone();
                self.ned_suggestions
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), updated);

                let mut events = vec![ConductorEvent::NedSuggestionAccepted {
                    id,
                    file: accepted_file,
                    pages,
                }];
                if !failed.is_empty() {
                    events.push(ConductorEvent::BuildFailed {
                        error_pages: failed,
                    });
                }
                CommandResult::ok_with_events(events)
            }

            Command::RejectNedSuggestion { id } => {
                // Look up suggestion; must be Pending or Stale
                let sug = {
                    let map = self
                        .ned_suggestions
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    match map.get(&id) {
                        Some(s)
                            if matches!(
                                s.status,
                                editorial_types::NedSuggestionStatus::Pending
                                    | editorial_types::NedSuggestionStatus::Stale { .. }
                            ) =>
                        {
                            s.clone()
                        }
                        Some(_) => {
                            return CommandResult::error(format!(
                                "ned suggestion {id} is not pending or stale"
                            ));
                        }
                        None => {
                            return CommandResult::error(format!("ned suggestion not found: {id}"));
                        }
                    }
                };

                let mut updated = sug.clone();
                updated.status = editorial_types::NedSuggestionStatus::Rejected;
                if let Err(e) = self.persist_ned_suggestion(&updated) {
                    eprintln!("conductor: failed to persist rejected ned suggestion: {e}");
                }
                self.ned_suggestions
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), updated.clone());

                CommandResult::ok_with_events(vec![ConductorEvent::NedSuggestionRejected {
                    id,
                    file: updated.file.clone(),
                }])
            }

            Command::GetNedSuggestions { file } => {
                let map = self
                    .ned_suggestions
                    .read()
                    .unwrap_or_else(|e| e.into_inner());
                let result: Vec<editorial_types::NedSuggestion> =
                    map.values().filter(|s| s.file == file).cloned().collect();
                CommandResult::with_response(Response::NedSuggestions(result))
            }

            // ── Phase D T4 stub: schema render endpoint ───────────────────────
            Command::RenderPage { path, mode } => match self.render_page(&path, &mode) {
                Ok(html) => CommandResult::with_response(Response::PageRendered { html }),
                Err(e) => CommandResult::error(e),
            },

            // ── Phase D Slice 1.5 Wave A: schema URL routing ─────────────────
            Command::SchemaUrlForPage { page_url } => CommandResult::with_response(
                Response::SchemaUrl(self.schema_url_for_page(&page_url)),
            ),

            Command::PageUrlForSchema { schema_url } => CommandResult::with_response(
                Response::PageUrl(self.page_url_for_schema(&schema_url)),
            ),
        }
    }
}

#[cfg(test)]
mod query_edges_tests {
    use super::*;
    use std::path::PathBuf;

    /// Where the test document's PathRef link expression sits.
    #[derive(Clone, Copy)]
    enum Placement {
        PreambleSlot,
        Body,
    }

    /// Build a conductor with a document in the NodeStore that contains a
    /// link-expression with a path-ref target.
    fn make_conductor_with_link_expression(
        source_url: &str,
        target_url: &str,
        placement: Placement,
    ) -> Conductor {
        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap();

        let link = schema::Spanned {
            node: content::ContentElement::LinkExpression {
                text: content::LinkText::Empty,
                target: content::LinkTarget::PathRef(target_url.to_string()),
                slot: None,
            },
            span: schema::Span { start: 0, end: 0 },
        };
        let (preamble, body) = match placement {
            Placement::PreambleSlot => (
                im::vector![content::DocumentSlot {
                    name: schema::SlotName::new("author"),
                    elements: im::vector![link],
                }],
                im::vector![],
            ),
            Placement::Body => (im::vector![], im::vector![link]),
        };
        let doc = content::Document {
            preamble,
            body,
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };

        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: source_url.to_string(),
            stem: "post".to_string(),
            file: format!(
                "content/post/{}.md",
                source_url.rsplit('/').next().unwrap_or("x")
            ),
            page_kind: "item".to_string(),
        };

        let mut store = conductor.node_store.write().unwrap();
        let root =
            node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
        drop(store);

        conductor
            .url_to_root
            .write()
            .unwrap()
            .insert(source_url.to_string(), root);
        conductor.path_to_doc_root.write().unwrap().insert(
            PathBuf::from(format!(
                "content/post/{}.md",
                source_url.rsplit('/').next().unwrap_or("x")
            )),
            root,
        );

        conductor
    }

    /// Build a conductor with a document that has no link-expression.
    fn make_conductor_with_no_link_expression(source_url: &str) -> Conductor {
        let repo = site_repository::SiteRepository::builder().build();
        let conductor = Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap();

        let doc = content::Document {
            preamble: im::vector![],
            body: im::vector![schema::Spanned {
                node: content::ContentElement::paragraph_from_markdown("No links here."),
                span: schema::Span { start: 0, end: 0 },
            }],
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };

        let meta = node_store_bridge::content_bridge::DocumentMeta {
            url: source_url.to_string(),
            stem: "post".to_string(),
            file: format!(
                "content/post/{}.md",
                source_url.rsplit('/').next().unwrap_or("x")
            ),
            page_kind: "item".to_string(),
        };

        let mut store = conductor.node_store.write().unwrap();
        let root =
            node_store_bridge::content_bridge::document_to_store(&doc, &mut store, Some(&meta));
        drop(store);

        conductor.insert_url_root(source_url, root);

        conductor
    }

    /// REPL expectation change (package G): only declared references count,
    /// and body references wait for ADR-059, so a body PathRef is no edge.
    #[test]
    fn query_edges_ignore_body_path_refs() {
        let conductor =
            make_conductor_with_link_expression("/post/alpha", "/author/alice", Placement::Body);
        assert!(conductor.query_edges_to("/author/alice").is_empty());
        assert!(conductor.query_edges_from("/post/alpha").is_empty());
    }

    #[test]
    fn query_edges_normalise_path_ref_targets() {
        let conductor = make_conductor_with_link_expression(
            "/post/alpha",
            "author/alice",
            Placement::PreambleSlot,
        );
        let edges = conductor.query_edges_to("/author/alice");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_to_finds_path_ref_link_expressions() {
        let conductor = make_conductor_with_link_expression(
            "/post/alpha",
            "/author/alice",
            Placement::PreambleSlot,
        );

        let edges = conductor.query_edges_to("/author/alice");
        assert_eq!(
            edges.len(),
            1,
            "expected 1 edge to /author/alice from link-expression, got {}",
            edges.len()
        );
        assert_eq!(edges[0].source, site_index::UrlPath::new("/post/alpha"));
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_from_finds_path_ref_link_expressions() {
        let conductor = make_conductor_with_link_expression(
            "/post/alpha",
            "/author/alice",
            Placement::PreambleSlot,
        );

        let edges = conductor.query_edges_from("/post/alpha");
        assert_eq!(
            edges.len(),
            1,
            "expected 1 edge from /post/alpha via link-expression, got {}",
            edges.len()
        );
        assert_eq!(edges[0].source, site_index::UrlPath::new("/post/alpha"));
        assert_eq!(edges[0].target, site_index::UrlPath::new("/author/alice"));
    }

    #[test]
    fn query_edges_to_no_false_positives_for_documents_without_links() {
        // A document with no link-expression should NOT produce any edges
        let conductor = make_conductor_with_no_link_expression("/post/beta");

        let edges = conductor.query_edges_to("/any/target");
        assert!(
            edges.is_empty(),
            "documents without link-expressions should not produce edges"
        );
    }
}

#[cfg(test)]
mod smoke_tests {
    use super::*;

    const SCHEMA_SRC: &str = "# Your post title {#title}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
    const CONTENT_SRC: &str = "title: Hello World\n---\nBody text here\n";

    /// Build a minimal site in a tempdir and return the tempdir.
    ///
    /// Layout:
    ///   schemas/post/item.md      — a simple schema with a title slot
    ///   templates/post/item.hiccup — a minimal hiccup template
    ///   content/post/hello.md     — a content file with title and body
    fn build_minimal_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC)
            .expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_SRC).expect("write content");

        tmp
    }

    /// Create a conductor for the given tempdir using the builder-based repo
    /// so the schema cache is populated from disk.
    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    #[test]
    fn classify_absolute_content_path() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::Classify {
            path: editorial_types::SitePath::parse("content/post/hello.md").expect("valid"),
        };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Content { ref schema_stem })
                if schema_stem == "post"
            ),
            "expected Content classification with schema_stem=post, got {:?}",
            result.response
        );
    }

    #[test]
    fn classify_absolute_template_path() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::Classify {
            path: editorial_types::SitePath::parse("templates/post/item.hiccup").expect("valid"),
        };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Template { ref schema_stem })
                if schema_stem == "post"
            ),
            "expected Template classification with schema_stem=post, got {:?}",
            result.response
        );
    }

    #[test]
    fn classify_absolute_schema_path() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::Classify {
            path: editorial_types::SitePath::parse("schemas/post/item.md").expect("valid"),
        };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Schema { ref stem })
                if stem == "post"
            ),
            "expected Schema classification with stem=post, got {:?}",
            result.response
        );
    }

    #[test]
    fn classify_a_path_that_is_no_site_file_returns_unknown() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::Classify {
            path: editorial_types::SitePath::parse("not-a-site/foo.md").expect("valid"),
        };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(
                result.response,
                Response::FileClassification(FileClassification::Unknown)
            ),
            "expected Unknown classification for path outside site, got {:?}",
            result.response
        );
    }

    #[test]
    fn get_schema_source_after_construction() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        let cmd = Command::GetGrammar {
            stem: "post".into(),
        };
        let result = conductor.handle_command(cmd);
        assert!(
            matches!(result.response, Response::SchemaSource(Some(_))),
            "expected SchemaSource(Some(_)) for known stem, got {:?}",
            result.response
        );
    }

    #[test]
    fn completions_flow_classify_then_schema() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);

        // Step 1: classify the content file (as the LSP does)
        let classify_cmd = Command::Classify {
            path: editorial_types::SitePath::parse("content/post/hello.md").expect("valid"),
        };
        let classify_result = conductor.handle_command(classify_cmd);

        let stem = match classify_result.response {
            Response::FileClassification(FileClassification::Content { schema_stem }) => {
                schema_stem
            }
            other => panic!("expected Content classification, got {:?}", other),
        };
        assert_eq!(stem, "post");

        // Step 2: get schema source for that stem
        let grammar_cmd = Command::GetGrammar { stem: stem.clone() };
        let grammar_result = conductor.handle_command(grammar_cmd);

        let schema_src = match grammar_result.response {
            Response::SchemaSource(Some(src)) => src,
            other => panic!("expected SchemaSource(Some(_)), got {:?}", other),
        };

        // Step 3: parse schema and verify the title slot is present
        let grammar = schema::parse_schema(&schema_src).expect("schema should parse successfully");

        assert!(
            grammar
                .preamble
                .iter()
                .any(|slot| slot.name.as_str() == "title"),
            "grammar should have a 'title' slot; preamble slots: {:?}",
            grammar
                .preamble
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod validation_report_tests {
    use super::*;

    const ITEM_SCHEMA_SRC: &str = "# Post title {#title}\noccurs\n: exactly once\n";
    const COLLECTION_SCHEMA_SRC: &str = "# Heading {#heading}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div]";

    /// A valid site under `<tmp>/site` (so the output dir `<tmp>/output/site`
    /// stays inside the tempdir): a post schema with two items and a
    /// collection page.
    fn valid_site() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let site = tmp.path().join("site");
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(site.join(dir)).expect("create dir");
        }
        let files = [
            ("schemas/post/item.md", ITEM_SCHEMA_SRC),
            ("schemas/post/index.md", COLLECTION_SCHEMA_SRC),
            ("templates/post/item.hiccup", TEMPLATE_SRC),
            ("templates/post/index.hiccup", TEMPLATE_SRC),
            ("content/post/hello.md", "# Hello\n"),
            ("content/post/second.md", "# Second\n"),
            ("content/post/index.md", "# Posts\n"),
        ];
        for (path, src) in files {
            std::fs::write(site.join(path), src).expect("write file");
        }
        (tmp, site)
    }

    fn errors_for(
        report: &validation::ValidationReport,
        file: &str,
    ) -> Vec<validation::SourceDiagnostic> {
        report
            .iter()
            .filter(|d| d.file == Path::new(file) && d.severity == validation::Severity::Error)
            .cloned()
            .collect()
    }

    #[test]
    fn valid_site_has_no_errors() {
        let (_tmp, site) = valid_site();
        let report = Conductor::load(site)
            .expect("conductor")
            .validation_report();
        assert!(
            !report.has_errors(),
            "expected no errors, got: {:#?}",
            report.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn invalid_item_reports_error_at_its_file() {
        let (_tmp, site) = valid_site();
        // An H2 where the title slot needs an H1.
        std::fs::write(site.join("content/post/x.md"), "## Not a title\n").expect("write");
        let report = Conductor::load(site)
            .expect("conductor")
            .validation_report();

        let errors = errors_for(&report, "content/post/x.md");
        assert!(
            !errors.is_empty(),
            "expected errors for x.md, got: {:#?}",
            report.iter().collect::<Vec<_>>()
        );
        assert!(
            errors.iter().any(|d| d.at.is_some()),
            "expected a positioned error, got: {errors:#?}"
        );
        assert!(report.has_errors());
        assert_eq!(report.files_with_errors(), 1);
    }

    #[test]
    fn unparseable_schema_reports_schema_and_not_built_pages() {
        let (_tmp, site) = valid_site();
        std::fs::write(
            site.join("schemas/post/item.md"),
            "# Title {#title}\ncontent\n: shouty\n",
        )
        .expect("write");
        let report = Conductor::load(site)
            .expect("conductor")
            .validation_report();

        let schema_errors = errors_for(&report, "schemas/post/item.md");
        assert_eq!(schema_errors.len(), 1, "got: {schema_errors:#?}");
        assert!(schema_errors[0].at.is_none());

        for file in ["content/post/hello.md", "content/post/second.md"] {
            let errors = errors_for(&report, file);
            assert_eq!(errors.len(), 1, "{file}: {errors:#?}");
            assert_eq!(
                errors[0].message,
                "not built: schema `schemas/post/item.md` does not parse"
            );
        }
        // The collection page has its own (valid) schema.
        assert!(errors_for(&report, "content/post/index.md").is_empty());
    }

    #[test]
    fn broken_collection_schema_does_not_fall_back_to_item_grammar() {
        let (_tmp, site) = valid_site();
        std::fs::write(
            site.join("schemas/post/index.md"),
            "# Title {#title}\ncontent\n: shouty\n",
        )
        .expect("write");
        let conductor = Conductor::load(site).expect("conductor");
        let report = conductor.validation_report();

        let errors = errors_for(&report, "content/post/index.md");
        assert_eq!(errors.len(), 1, "got: {errors:#?}");
        assert_eq!(
            errors[0].message,
            "not built: schema `schemas/post/index.md` does not parse"
        );
        assert!(
            conductor.document_by_url("/post/").is_none(),
            "the collection page must not be stored"
        );
    }

    #[test]
    fn load_writes_nothing_to_output_dir() {
        let (_tmp, site) = valid_site();
        let conductor = Conductor::load(site).expect("conductor");
        let output = conductor.output_dir.clone();
        let written = std::fs::read_dir(&output).map(|d| d.count()).unwrap_or(0);
        assert_eq!(written, 0, "expected nothing under {}", output.display());
    }
}

#[cfg(test)]
mod populate_collection_tests {
    use super::*;

    const ITEM_SCHEMA_SRC: &str = "# Post title {#title}\noccurs\n: exactly once\n";
    const COLLECTION_SCHEMA_SRC: &str = "# Heading {#heading}\noccurs\n: exactly once\n";
    const ITEM_TEMPLATE_SRC: &str = "[:div]";
    const COLLECTION_TEMPLATE_SRC: &str = "[:div]";
    const ITEM_CONTENT_SRC: &str = "title: Hello\n";
    const COLLECTION_CONTENT_SRC: &str = "heading: Posts\n";

    /// Create a conductor for a tempdir (filesystem-backed).
    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    // -------------------------------------------------------------------------
    // Test 1: collection_page_in_graph
    // -------------------------------------------------------------------------

    /// Build a site with a post schema, item content, and a collection index.
    ///
    /// After populating the node store, `/post/` must be indexed as a
    /// collection page.
    fn build_site_with_collection() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        // Item schema + template + content
        std::fs::write(root.join("schemas/post/item.md"), ITEM_SCHEMA_SRC)
            .expect("write item schema");
        std::fs::write(root.join("templates/post/item.hiccup"), ITEM_TEMPLATE_SRC)
            .expect("write item template");
        std::fs::write(root.join("content/post/hello.md"), ITEM_CONTENT_SRC)
            .expect("write item content");

        // Collection schema + template + content
        std::fs::write(root.join("schemas/post/index.md"), COLLECTION_SCHEMA_SRC)
            .expect("write collection schema");
        std::fs::write(
            root.join("templates/post/index.hiccup"),
            COLLECTION_TEMPLATE_SRC,
        )
        .expect("write collection template");
        std::fs::write(root.join("content/post/index.md"), COLLECTION_CONTENT_SRC)
            .expect("write collection content");

        tmp
    }

    #[test]
    fn collection_page_in_graph() {
        let tmp = build_site_with_collection();
        let conductor = make_conductor(&tmp);

        // Verify via NodeStore that /post/ is indexed as a collection page
        let root = conductor
            .document_by_url("/post/")
            .unwrap_or_else(|| panic!("expected node at /post/ in NodeStore"));

        let store = conductor.node_store.read().unwrap();
        let page_kind =
            node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind");
        assert_eq!(
            page_kind.as_deref(),
            Some("collection"),
            "node at /post/ should have page-kind=collection, got {:?}",
            page_kind
        );
    }

    // -------------------------------------------------------------------------
    // Test 2: root_collection_page_in_graph
    // -------------------------------------------------------------------------

    fn build_root_collection_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content")).expect("create content");

        // Root collection schema, content, template
        std::fs::write(root.join("schemas/index.md"), COLLECTION_SCHEMA_SRC)
            .expect("write root schema");
        std::fs::write(root.join("templates/index.hiccup"), COLLECTION_TEMPLATE_SRC)
            .expect("write root template");
        std::fs::write(root.join("content/index.md"), "heading: Home\n")
            .expect("write root content");

        tmp
    }

    #[test]
    fn root_collection_page_in_graph() {
        let tmp = build_root_collection_site();
        let conductor = make_conductor(&tmp);

        // Verify via NodeStore that / is indexed as a collection page
        let root = conductor
            .document_by_url("/")
            .unwrap_or_else(|| panic!("expected node at / in NodeStore"));

        let store = conductor.node_store.read().unwrap();
        let page_kind =
            node_store_bridge::content_bridge::find_attr_text(&store, root, "page-kind");
        assert_eq!(
            page_kind.as_deref(),
            Some("collection"),
            "node at / should have page-kind=collection, got {:?}",
            page_kind
        );
    }

    // -------------------------------------------------------------------------
    // Test 3: legacy_fallback_root_page
    // -------------------------------------------------------------------------

    /// A site with item pages and a root collection template but NO root schema
    /// or root content. After populating the node store the root `/` node should still
    /// appear (legacy fallback behaviour).
    fn build_legacy_fallback_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        // Item schema + content (no collection schema/content for root)
        std::fs::write(root.join("schemas/post/item.md"), ITEM_SCHEMA_SRC)
            .expect("write item schema");
        std::fs::write(root.join("content/post/hello.md"), ITEM_CONTENT_SRC)
            .expect("write item content");

        // Root collection template only — no root schema or content
        std::fs::write(root.join("templates/index.hiccup"), COLLECTION_TEMPLATE_SRC)
            .expect("write root template");

        tmp
    }

    #[test]
    fn legacy_fallback_root_page() {
        let tmp = build_legacy_fallback_site();
        let conductor = make_conductor(&tmp);

        // Legacy fallback: if there's no root content, the NodeStore may not have /
        // but the build pipeline should still run without panicking.
        // The important thing is the conductor constructed successfully and the post
        // content was indexed.
        let roots = conductor.documents_for_stem("post");
        assert!(
            !roots.is_empty(),
            "expected post content to be indexed in NodeStore"
        );
    }

    // -------------------------------------------------------------------------
    // Test 4: collection_deps_include_item_content
    // -------------------------------------------------------------------------

    /// The NodeStore should index both the collection and item documents for "post".
    #[test]
    fn collection_deps_include_item_content() {
        let tmp = build_site_with_collection();
        let conductor = make_conductor(&tmp);

        // Both the collection /post/ and item /post/hello should be in the NodeStore
        let collection_root = conductor.document_by_url("/post/");
        assert!(
            collection_root.is_some(),
            "expected /post/ to be indexed in NodeStore"
        );

        let item_root = conductor.document_by_url("/post/hello");
        assert!(
            item_root.is_some(),
            "expected /post/hello to be indexed in NodeStore"
        );

        // The collection and item are both under the "post" stem
        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            2,
            "expected 2 documents for stem 'post' (item + collection), got {}",
            roots.len()
        );
    }
}

#[cfg(test)]
mod node_store_index_tests {
    use super::*;

    const SCHEMA_SRC: &str = "# Your post title {#title}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
    // Use real markdown heading format so the title slot receives just "Hello World"
    // (not "title: Hello World" from a setext-heading side-effect).
    const CONTENT_SRC: &str = "# Hello World\n\n----\n\nBody text here\n";
    const CONTENT_SRC2: &str = "# Second Post\n\n----\n\nMore body text\n";

    fn build_site_with_two_posts() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC)
            .expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_SRC).expect("write content 1");
        std::fs::write(root.join("content/post/second.md"), CONTENT_SRC2).expect("write content 2");

        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    #[test]
    fn document_by_url_finds_item() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/post/hello");
        assert!(
            root.is_some(),
            "document_by_url('/post/hello') should find a NodeId after populate_node_store"
        );
    }

    #[test]
    fn document_by_url_returns_none_for_unknown() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor.document_by_url("/nonexistent/page");
        assert!(
            root.is_none(),
            "document_by_url should return None for unknown URL"
        );
    }

    #[test]
    fn documents_for_stem_finds_all_items() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            2,
            "documents_for_stem('post') should return 2 roots for two content files, got {}",
            roots.len()
        );
    }

    #[test]
    fn documents_for_stem_returns_empty_for_unknown() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let roots = conductor.documents_for_stem("nonexistent");
        assert!(
            roots.is_empty(),
            "documents_for_stem should return empty vec for unknown stem"
        );
    }

    #[test]
    fn datagraph_for_document_returns_title() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor
            .document_by_url("/post/hello")
            .expect("should find /post/hello in index");

        let data = conductor
            .datagraph_for_document(root)
            .expect("datagraph_for_document should succeed");

        match data.resolve(&["title"]) {
            Some(v @ template::Value::Inlines(_)) => assert_eq!(
                v.display_text().as_deref(),
                Some("Hello World"),
                "title should be 'Hello World', got {v:?}"
            ),
            other => panic!("expected the title as inlines, got {other:?}"),
        }
    }

    #[test]
    fn datagraph_for_document_injects_url_and_link() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let root = conductor
            .document_by_url("/post/hello")
            .expect("should find /post/hello in index");

        let data = conductor
            .datagraph_for_document(root)
            .expect("datagraph_for_document should succeed");

        // url field
        assert!(
            matches!(data.resolve(&["url"]), Some(template::Value::Text(u)) if u == "/post/hello"),
            "url field should be '/post/hello'"
        );

        // link record should be present with href
        assert!(
            matches!(data.resolve(&["link"]), Some(template::Value::Record(_))),
            "link field should be a Record"
        );
    }

    #[test]
    fn query_items_from_store_returns_all_items() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let items = conductor.query_items_from_store("post");
        assert_eq!(
            items.len(),
            2,
            "query_items_from_store('post') should return 2 items, got {}",
            items.len()
        );

        // Both URLs should be present
        let urls: Vec<&str> = items.iter().map(|(u, _)| u.as_str()).collect();
        assert!(
            urls.contains(&"/post/hello"),
            "items should include /post/hello; got {:?}",
            urls
        );
        assert!(
            urls.contains(&"/post/second"),
            "items should include /post/second; got {:?}",
            urls
        );
    }

    #[test]
    fn query_items_from_store_returns_empty_for_unknown_stem() {
        let tmp = build_site_with_two_posts();
        let conductor = make_conductor(&tmp);

        let items = conductor.query_items_from_store("nonexistent");
        assert!(
            items.is_empty(),
            "query_items_from_store should return empty vec for unknown stem"
        );
    }

    // -------------------------------------------------------------------------
    // _presemble_file injection tests (Bug 2 Layer 1)
    // -------------------------------------------------------------------------

    /// Site with content/index.md (stem = ""). Verifies that datagraph_for_document
    /// injects _presemble_file = "content/index.md" for the root index page.
    fn build_root_index_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content")).expect("create content");

        // Root index schema (heading slot)
        std::fs::write(
            root.join("schemas/index.md"),
            "# Heading {#heading}\noccurs\n: exactly once\n",
        )
        .expect("write root schema");
        std::fs::write(root.join("templates/index.hiccup"), "[:div [:h1 heading]]")
            .expect("write root template");
        std::fs::write(root.join("content/index.md"), "heading: Home\n")
            .expect("write root content");

        tmp
    }

    /// Site with a root template but NO content/index.md (legacy fallback root).
    /// In this case the conductor synthesises a dummy node with file = "".
    fn build_legacy_root_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        // A post item so the conductor has something to index
        std::fs::write(
            root.join("schemas/post/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .expect("write item schema");
        std::fs::write(root.join("content/post/hello.md"), "title: Hello\n")
            .expect("write item content");
        // Root template only — no schema, no content/index.md
        std::fs::write(root.join("templates/index.hiccup"), "[:div]").expect("write root template");

        tmp
    }

    #[test]
    fn datagraph_for_document_injects_presemble_file_for_root_index() {
        let tmp = build_root_index_site();
        let conductor = make_conductor(&tmp);

        let root = conductor
            .document_by_url("/")
            .expect("should find / in NodeStore after building root index site");

        let data = conductor
            .datagraph_for_document(root)
            .expect("datagraph_for_document should succeed for root index");

        match data.resolve(&["_presemble_file"]) {
            Some(template::Value::Text(f)) => assert_eq!(
                f, "content/index.md",
                "_presemble_file should be 'content/index.md' for root index, got {f:?}"
            ),
            other => panic!("expected _presemble_file Text value, got {other:?}"),
        }
    }

    #[test]
    fn datagraph_for_document_injects_presemble_file_for_legacy_fallback_root() {
        let tmp = build_legacy_root_site();
        let conductor = make_conductor(&tmp);

        // Legacy fallback creates a synthetic node at / with file = ""
        let root = conductor
            .document_by_url("/")
            .expect("should find / in NodeStore (legacy fallback creates synthetic root)");

        let data = conductor
            .datagraph_for_document(root)
            .expect("datagraph_for_document should succeed for legacy fallback root");

        match data.resolve(&["_presemble_file"]) {
            Some(template::Value::Text(f)) => assert_eq!(
                f, "content/index.md",
                "_presemble_file should be 'content/index.md' for legacy fallback root (stem=''), got {f:?}"
            ),
            other => panic!(
                "expected _presemble_file Text value for legacy fallback root, got {other:?}"
            ),
        }
    }
}

#[cfg(test)]
mod update_document_in_store_tests {
    use super::*;

    const SCHEMA_SRC: &str = "# Post title {#title}\noccurs\n: exactly once\n";
    const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
    const CONTENT_V1: &str = "# Original Title\n\n----\n\nOriginal body\n";
    const CONTENT_V2: &str = "# Updated Title\n\n----\n\nUpdated body\n";

    fn build_single_post_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas/post");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates/post");
        std::fs::create_dir_all(root.join("content/post")).expect("create content/post");

        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC)
            .expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_V1).expect("write content");

        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    /// After calling update_document_in_store, the url_to_root map should
    /// point to the new document root NodeId.
    #[test]
    fn update_document_sets_url_to_root() {
        let tmp = build_single_post_site();
        let conductor = make_conductor(&tmp);

        let old_root = conductor
            .document_by_url("/post/hello")
            .expect("initial populate_node_store should index /post/hello");

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        let new_sem = conductor
            .update_document_in_store("/post/hello", "post", "hello", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let new_root = conductor
            .document_by_url("/post/hello")
            .expect("url_to_root should still contain /post/hello after update");

        // The new root must differ from the old one (re-parse creates new nodes)
        assert_ne!(
            old_root, new_root,
            "update_document_in_store should create new document nodes (old_root={old_root:?}, new_root={new_root:?})"
        );

        // The returned sem NodeId must equal what url_to_semantic holds
        let sem_from_index = conductor
            .semantic_content_by_url("/post/hello")
            .expect("url_to_semantic should contain /post/hello after update");
        assert_eq!(
            new_sem, sem_from_index,
            "returned NodeId should match url_to_semantic entry"
        );
    }

    /// After update_document_in_store, stem_to_roots should contain exactly
    /// one entry for the updated document's stem+slug (no duplicate roots).
    #[test]
    fn update_document_does_not_duplicate_stem_roots() {
        let tmp = build_single_post_site();
        let conductor = make_conductor(&tmp);

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        conductor
            .update_document_in_store("/post/hello", "post", "hello", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let roots = conductor.documents_for_stem("post");
        assert_eq!(
            roots.len(),
            1,
            "stem_to_roots['post'] should have exactly 1 root after update (no duplicates), got {}",
            roots.len()
        );
    }

    #[test]
    fn documents_for_stem_stays_in_url_order_after_an_edit() {
        let tmp = build_single_post_site();
        let dir = tmp.path().join("content/post");
        std::fs::remove_file(dir.join("hello.md")).expect("remove hello.md");
        for slug in ["a", "b", "c"] {
            std::fs::write(dir.join(format!("{slug}.md")), CONTENT_V1).expect("write post");
        }
        let conductor = make_conductor(&tmp);

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        conductor
            .update_document_in_store("/post/a", "post", "a", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let roots = conductor.documents_for_stem("post");
        let store = conductor.node_store.read().unwrap();
        let urls: Vec<String> = roots
            .iter()
            .filter_map(|&r| node_store_bridge::content_bridge::find_attr_text(&store, r, "url"))
            .collect();
        assert_eq!(urls, ["/post/a", "/post/b", "/post/c"]);
    }

    #[test]
    fn root_stem_item_is_not_duplicated_by_an_edit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("schemas")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates")).expect("create templates");
        std::fs::create_dir_all(root.join("content")).expect("create content");
        std::fs::write(root.join("schemas/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/item.hiccup"), TEMPLATE_SRC).expect("write template");
        std::fs::write(root.join("content/about.md"), CONTENT_V1).expect("write content");
        let conductor = make_conductor(&tmp);

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        conductor
            .update_document_in_store("/about", "", "about", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let roots = conductor.documents_for_stem("");
        let new_root = conductor
            .document_by_url("/about")
            .expect("/about should be indexed");
        assert_eq!(
            roots,
            vec![new_root],
            "root stem must list the edited item exactly once"
        );
    }

    /// The new document root should reflect the updated document text.
    /// We verify this by checking that datagraph_for_document on the new root
    /// returns the updated title.
    #[test]
    fn update_document_reflects_new_content() {
        let tmp = build_single_post_site();
        let conductor = make_conductor(&tmp);

        let grammar = schema::parse_schema(SCHEMA_SRC).expect("parse schema");
        conductor
            .update_document_in_store("/post/hello", "post", "hello", CONTENT_V2, &grammar)
            .expect("update_document_in_store should succeed");

        let new_root = conductor
            .document_by_url("/post/hello")
            .expect("should find updated document");

        let data = conductor
            .datagraph_for_document(new_root)
            .expect("datagraph_for_document should succeed on new root");

        match data.resolve(&["title"]) {
            Some(v @ template::Value::Inlines(_)) => assert_eq!(
                v.display_text().as_deref(),
                Some("Updated Title"),
                "title should reflect updated content, got {v:?}"
            ),
            other => panic!("expected the title as inlines, got {other:?}"),
        }
    }

    // ── ADR-056 exact addressing: a re-ingested document must not leave a
    //    second root carrying the same `file` / `url` attributes ──────────────

    const OTHER_CONTENT: &str = "# Other Title\n\n----\n\nOther body\n";

    fn build_two_post_site() -> tempfile::TempDir {
        let tmp = build_single_post_site();
        std::fs::write(tmp.path().join("content/post/other.md"), OTHER_CONTENT)
            .expect("write other content");
        // rebuild_page renders, so it needs a template that parses.
        std::fs::write(tmp.path().join("templates/post/item.hiccup"), "[:div]")
            .expect("write renderable template");
        tmp
    }

    fn eval_count(conductor: &Conductor, program: &str) -> i64 {
        let root = conductor
            .make_ned_root(&evaluator::ned_primitives::PlanRecorder::new())
            .expect("make_ned_root");
        match evaluator::eval_str_with_root(program, &root) {
            Ok(template::Value::Integer(n)) => n,
            other => panic!("expected Integer from {program}, got {other:?}"),
        }
    }

    fn rebuild_hello_twice(conductor: &Conductor, tmp: &tempfile::TempDir) {
        let path = tmp.path().join("content/post/hello.md");
        conductor
            .rebuild_page(&path, CONTENT_V2)
            .expect("first rebuild_page");
        conductor
            .rebuild_page(&path, CONTENT_V1)
            .expect("second rebuild_page");
    }

    #[test]
    fn rebuild_page_twice_leaves_single_doc_by_path() {
        let tmp = build_two_post_site();
        let conductor = make_conductor(&tmp);
        rebuild_hello_twice(&conductor, &tmp);

        assert_eq!(
            eval_count(
                &conductor,
                r#"(ned/count (ned/doc-by-path "content/post/hello.md"))"#
            ),
            1,
            "doc-by-path must resolve to exactly one document after repeated rebuilds"
        );
    }

    #[test]
    fn rebuild_page_twice_leaves_single_by_url() {
        let tmp = build_two_post_site();
        let conductor = make_conductor(&tmp);
        rebuild_hello_twice(&conductor, &tmp);

        assert_eq!(
            eval_count(&conductor, r#"(ned/count (ned/by-url "/post/hello"))"#),
            1,
            "by-url must resolve to exactly one document after repeated rebuilds"
        );
    }

    #[test]
    fn rebuild_page_twice_indexes_point_at_live_root() {
        let tmp = build_two_post_site();
        let conductor = make_conductor(&tmp);
        rebuild_hello_twice(&conductor, &tmp);

        let by_url = conductor
            .document_by_url("/post/hello")
            .expect("url_to_root entry");
        let by_path = conductor
            .doc_root_for_path(Path::new("content/post/hello.md"))
            .expect("path_to_doc_root entry");
        assert_eq!(
            by_url, by_path,
            "url_to_root and path_to_doc_root must agree"
        );

        let store = conductor.node_store.read().unwrap();
        assert_eq!(
            node_store_bridge::content_bridge::find_attr_text(&store, by_path, "file").as_deref(),
            Some("content/post/hello.md"),
            "indexed root must still carry its file attribute"
        );
        assert_eq!(
            node_store_bridge::content_bridge::find_attr_text(&store, by_path, "url").as_deref(),
            Some("/post/hello"),
            "indexed root must still carry its url attribute"
        );
    }

    #[test]
    fn rebuild_page_twice_leaves_other_documents_unchanged() {
        let tmp = build_two_post_site();
        let conductor = make_conductor(&tmp);

        let other_before = conductor
            .document_by_url("/post/other")
            .expect("other indexed");
        rebuild_hello_twice(&conductor, &tmp);
        let other_after = conductor
            .document_by_url("/post/other")
            .expect("other still indexed");

        assert_eq!(
            other_before, other_after,
            "other document root must not change"
        );
        assert_eq!(
            eval_count(
                &conductor,
                r#"(ned/count (ned/doc-by-path "content/post/other.md"))"#
            ),
            1
        );
        assert_eq!(
            eval_count(&conductor, r#"(ned/count (ned/by-url "/post/other"))"#),
            1
        );
        let store = conductor.node_store.read().unwrap();
        assert_eq!(
            node_store_bridge::content_bridge::find_attr_text(&store, other_after, "file")
                .as_deref(),
            Some("content/post/other.md")
        );
    }

    #[test]
    fn rebuild_page_twice_hides_superseded_roots_from_document_queries() {
        let tmp = build_two_post_site();
        let conductor = make_conductor(&tmp);

        let documents_before = eval_count(&conductor, "(ned/count (ned/all-documents))");
        rebuild_hello_twice(&conductor, &tmp);

        assert_eq!(
            eval_count(&conductor, "(ned/count (ned/all-documents))"),
            documents_before,
            "superseded roots must not show up in all-documents"
        );
        assert_eq!(
            eval_count(&conductor, "(ned/count (ned/posts))"),
            2,
            "posts must list each post exactly once after repeated rebuilds"
        );
    }

    #[test]
    fn superseded_root_keeps_identity_attributes() {
        let tmp = build_two_post_site();
        let conductor = make_conductor(&tmp);

        let old_root = conductor
            .document_by_url("/post/hello")
            .expect("hello indexed");
        rebuild_hello_twice(&conductor, &tmp);

        let store = conductor.node_store.read().unwrap();
        match store.get(old_root) {
            Some(node_store::Node::Element(name)) => assert_eq!(
                store.resolve_name(*name),
                "superseded-document",
                "old root must be retired, not left as a live document"
            ),
            other => panic!("expected old root to remain an Element, got {other:?}"),
        }
        assert_eq!(
            node_store_bridge::content_bridge::find_attr_text(&store, old_root, "url").as_deref(),
            Some("/post/hello"),
            "stale references into the old root must still read the correct url"
        );
        assert_eq!(
            node_store_bridge::content_bridge::find_attr_text(&store, old_root, "file").as_deref(),
            Some("content/post/hello.md"),
            "stale references into the old root must still read the correct file"
        );
    }
}

#[cfg(test)]
mod feature_card_rendering_tests {
    use std::collections::HashMap;

    // ── schemas ──────────────────────────────────────────────────────────────

    /// Schema for a feature item page.
    const FEATURE_SCHEMA_SRC: &str =
        "# Feature title {#title}\noccurs\n: exactly once\n\n----\n\nBody.\n";

    /// Schema for the root index page with a link slot referencing feature pages.
    const INDEX_SCHEMA_SRC: &str = "[<name>](/feature/<name>) {#highlight}\ntype\n: link(feature)\noccurs\n: 1..6\n\n----\n\nBody.\n";

    // ── content ──────────────────────────────────────────────────────────────

    const FEATURE_CONTENT_SRC: &str = "# Schemas As Contracts\n\n----\n\nFeature body.\n";

    const INDEX_CONTENT_SRC: &str =
        "[Schemas As Contracts](/feature/schemas-as-contracts)\n\n----\n\nIndex body.\n";

    // ── template ─────────────────────────────────────────────────────────────

    const INDEX_TEMPLATE_SRC: &str = r#"<ul><template data-each="(ref input.highlight)"><li><presemble:insert data="(ref item.title)" as="h3" /></li></template></ul>"#;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn node_type_name(store: &node_store::NodeStore, id: node_store::NodeId) -> String {
        match store.get(id) {
            Some(node_store::Node::Element(name)) => {
                format!("Element({})", store.resolve_name(*name))
            }
            Some(node_store::Node::Text(s)) => format!("Text({s:?})"),
            Some(node_store::Node::Collection) => "Collection".to_string(),
            Some(node_store::Node::Integer(n)) => format!("Integer({n})"),
            Some(node_store::Node::Boolean(b)) => format!("Boolean({b})"),
            Some(node_store::Node::Nil) => "Nil".to_string(),
            Some(node_store::Node::Keyword(name)) => {
                format!("Keyword({})", store.resolve_name(*name))
            }
            Some(node_store::Node::Opaque(_)) => "Opaque".to_string(),
            None => "Missing".to_string(),
        }
    }

    /// Print diagnostic state of the index semantic content node.
    fn debug_index_sem(store: &node_store::NodeStore, index_sem: node_store::NodeId) {
        eprintln!("=== DEBUG: index semantic content ({index_sem:?}) ===");

        let consists_of = store.consists_of(index_sem);
        eprintln!("  consists_of ({} entries):", consists_of.len());
        for (name, part) in &consists_of {
            let name_str = store.resolve_name(*name);
            let type_str = node_type_name(store, *part);
            eprintln!("    {name_str} -> {part:?} ({type_str})");
        }

        let references = store.references(index_sem);
        eprintln!("  references ({} entries):", references.len());
        for (name, target) in &references {
            let name_str = store.resolve_name(*name);
            let type_str = node_type_name(store, *target);
            eprintln!("    {name_str} -> {target:?} ({type_str})");
        }

        // Investigate the "highlight" slot specifically
        let highlight_id = consists_of
            .iter()
            .find(|(n, _)| store.resolve_name(*n) == "highlight")
            .map(|(_, id)| *id);
        let highlight_ref = references
            .iter()
            .find(|(n, _)| store.resolve_name(*n) == "highlight")
            .map(|(_, id)| *id);

        let highlight = highlight_id.or(highlight_ref);
        if let Some(h) = highlight {
            eprintln!("  highlight target: {h:?} ({})", node_type_name(store, h));
            let children = store.children(h);
            eprintln!("  highlight children ({}):", children.len());
            for child in &children {
                let ctype = node_type_name(store, *child);
                eprintln!("    {child:?} ({ctype})");
                // Walk the child's ConsistsOf edges
                let child_consists = store.consists_of(*child);
                if !child_consists.is_empty() {
                    for (n, p) in &child_consists {
                        let nstr = store.resolve_name(*n);
                        let ptype = node_type_name(store, *p);
                        eprintln!("      ConsistsOf {nstr} -> {p:?} ({ptype})");
                    }
                }
                // Also check for a "title" consists_of
                let child_refs = store.references(*child);
                if !child_refs.is_empty() {
                    for (n, p) in &child_refs {
                        let nstr = store.resolve_name(*n);
                        let ptype = node_type_name(store, *p);
                        eprintln!("      Reference {nstr} -> {p:?} ({ptype})");
                    }
                }
            }
        } else {
            eprintln!("  no 'highlight' edge found on index semantic content");
        }
    }

    // ── test ──────────────────────────────────────────────────────────────────

    /// Reproduce the feature card rendering failure from `presemble serve`.
    ///
    /// This test exercises the EXACT same code path:
    ///   content parsing -> document_to_store -> create_semantic_content ->
    ///   rewire_doc_refs_to_semantic -> PrefixedGraphView -> template::transform
    ///
    /// Expected to FAIL: feature cards are empty because the pipeline does not
    /// correctly wire the "highlight" link slot so that `item.title` is accessible
    /// inside `data-each`.
    #[test]
    fn feature_card_rendering_via_node_store() {
        // ── 1. Parse schemas ─────────────────────────────────────────────────
        let feature_grammar =
            schema::parse_schema(FEATURE_SCHEMA_SRC).expect("feature schema should parse");
        let index_grammar =
            schema::parse_schema(INDEX_SCHEMA_SRC).expect("index schema should parse");

        // ── 2. Parse content ─────────────────────────────────────────────────
        let feature_doc = content::parse_and_assign(FEATURE_CONTENT_SRC, &feature_grammar)
            .expect("feature content should parse");
        let index_doc = content::parse_and_assign(INDEX_CONTENT_SRC, &index_grammar)
            .expect("index content should parse");

        // ── 3. Store documents ───────────────────────────────────────────────
        let mut store = node_store::NodeStore::new();

        let feature_meta = node_store_bridge::content_bridge::DocumentMeta {
            url: "/feature/schemas-as-contracts".to_string(),
            stem: "feature".to_string(),
            file: "content/feature/schemas-as-contracts.md".to_string(),
            page_kind: "item".to_string(),
        };
        let feature_root = node_store_bridge::content_bridge::document_to_store(
            &feature_doc,
            &mut store,
            Some(&feature_meta),
        );

        let index_meta = node_store_bridge::content_bridge::DocumentMeta {
            url: "/".to_string(),
            stem: "".to_string(),
            file: "content/index.md".to_string(),
            page_kind: "collection".to_string(),
        };
        let index_root = node_store_bridge::content_bridge::document_to_store(
            &index_doc,
            &mut store,
            Some(&index_meta),
        );

        // ── 4. Build indexes ─────────────────────────────────────────────────
        let mut url_index: HashMap<String, node_store::NodeId> = HashMap::new();
        url_index.insert("/feature/schemas-as-contracts".to_string(), feature_root);
        url_index.insert("/".to_string(), index_root);

        let mut stem_index: HashMap<String, Vec<node_store::NodeId>> = HashMap::new();
        stem_index
            .entry("feature".to_string())
            .or_default()
            .push(feature_root);
        stem_index
            .entry("".to_string())
            .or_default()
            .push(index_root);

        // ── 5. Create semantic content ───────────────────────────────────────
        let mut sem_index: HashMap<String, node_store::NodeId> = HashMap::new();

        let feature_sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            feature_root,
            &feature_grammar,
            &feature_meta,
            &stem_index,
            &url_index,
            Some(&sem_index),
        );
        sem_index.insert("/feature/schemas-as-contracts".to_string(), feature_sem);

        let index_sem = node_store_bridge::content_bridge::create_semantic_content(
            &mut store,
            index_root,
            &index_grammar,
            &index_meta,
            &stem_index,
            &url_index,
            Some(&sem_index),
        );

        // ── 6. Render index page ─────────────────────────────────────────────
        let raw_nodes =
            template::parse_template_xml(INDEX_TEMPLATE_SRC).expect("template should parse");
        let (nodes, local_defs) = template::extract_definitions(raw_nodes);

        let reg = template::NullRegistry;
        let ctx = template::RenderContext::with_local_defs(
            &reg,
            &local_defs,
            template::OutputMode::Editorial,
        );

        let view = node_store_bridge::NodeStoreView::new(&store, index_sem);
        let prefixed = node_store_bridge::PrefixedGraphView::new("input".to_string(), view);

        let result = template::transform(nodes, &prefixed, &ctx);

        // ── 8. Assert and debug ──────────────────────────────────────────────
        match &result {
            Ok(transformed) => {
                let html = template::serialize_nodes(transformed);
                eprintln!("Rendered HTML: {html}");

                if !html.contains("Schemas As Contracts") {
                    // Print debug before failing
                    debug_index_sem(&store, index_sem);
                    panic!(
                        "Expected rendered HTML to contain 'Schemas As Contracts', but got:\n{html}"
                    );
                }
                // If we reach here the bug is fixed — the test passes.
            }
            Err(e) => {
                debug_index_sem(&store, index_sem);
                panic!("template::transform failed: {e:?}");
            }
        }
    }
}

#[cfg(test)]
mod workspace_hash_tests {
    use super::*;

    /// Create a minimal Conductor backed by a temporary directory that is NOT
    /// a git repo. Reuses the same `build_minimal_site` / `make_conductor`
    /// helpers from `smoke_tests`.
    fn build_minimal_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        const SCHEMA_SRC: &str = "# Title {#title}\noccurs\n: exactly once\n";
        const TEMPLATE_SRC: &str = "[:div [:h1 title]]";
        const CONTENT_SRC: &str = "title: Hello\n---\nBody\n";
        std::fs::create_dir_all(root.join("schemas/post")).expect("create schemas");
        std::fs::create_dir_all(root.join("templates/post")).expect("create templates");
        std::fs::create_dir_all(root.join("content/post")).expect("create content");
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA_SRC).expect("write schema");
        std::fs::write(root.join("templates/post/item.hiccup"), TEMPLATE_SRC)
            .expect("write template");
        std::fs::write(root.join("content/post/hello.md"), CONTENT_SRC).expect("write content");
        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    #[test]
    fn workspace_hash_returns_untracked_for_non_git_dir() {
        let tmp = build_minimal_site();
        let conductor = make_conductor(&tmp);
        // The tempdir has no `.git` directory — expect "untracked".
        assert_eq!(conductor.workspace_hash(), "untracked");
    }

    #[test]
    fn workspace_hash_returns_hex_for_git_repo() {
        use std::process::Command;

        // Skip the test if git is not available.
        let git_available = Command::new("git").arg("--version").output().is_ok();
        if !git_available {
            println!("skipping workspace_hash_returns_hex_for_git_repo: git not found");
            return;
        }

        let tmp = build_minimal_site();
        let root = tmp.path();

        // Initialise a git repo and create an empty commit.
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .expect("git command failed");
            assert!(status.success(), "git {args:?} exited with {status}");
        };

        git(&["init"]);
        git(&["commit", "--allow-empty", "-m", "init"]);

        let conductor = make_conductor(&tmp);
        let hash = conductor.workspace_hash();

        // A real git hash is exactly 40 hex characters.
        assert_eq!(hash.len(), 40, "expected 40-char hash, got: {hash:?}");
        assert!(
            hash.chars().all(|c| c.is_ascii_hexdigit()),
            "hash contains non-hex chars: {hash:?}"
        );
    }
}

#[cfg(test)]
mod creation_check_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    /// Build a minimal conductor with a document loaded into the NodeStore.
    ///
    /// The document at `content/test/doc.md` has:
    /// - a "title" slot containing a heading
    /// - a "link-slot" slot containing a link element
    /// - a "summary" slot containing two paragraphs
    /// - a body paragraph
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::heading_from_markdown(
                            HeadingLevel::new(1).unwrap(),
                            "Test Title",
                        ),
                        span: Span { start: 0, end: 0 },
                    }],
                },
                DocumentSlot {
                    name: SlotName::new("link-slot"),
                    elements: im::vector![Spanned {
                        node: ContentElement::lone_link("Click me", "https://example.com"),
                        span: Span { start: 0, end: 0 },
                    }],
                },
                DocumentSlot {
                    name: SlotName::new("summary"),
                    elements: im::vector![
                        Spanned {
                            node: ContentElement::paragraph_from_markdown("First."),
                            span: Span { start: 0, end: 0 },
                        },
                        Spanned {
                            node: ContentElement::paragraph_from_markdown("Second."),
                            span: Span { start: 0, end: 0 },
                        },
                    ],
                },
            ],
            body: im::vector![Spanned {
                node: ContentElement::paragraph_from_markdown("Body text"),
                span: Span { start: 0, end: 0 },
            },],
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };

        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: "content/test/doc.md".to_string(),
            page_kind: "item".to_string(),
        };

        {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }

        (conductor, tmp)
    }

    // -------------------------------------------------------------------------
    // Tests: creation-time checks (ADR-056), through `handle_command`
    // -------------------------------------------------------------------------

    const DOC: &str = "content/test/doc.md";

    fn create(
        conductor: &Conductor,
        file: &str,
        selection: &str,
        mutation: editorial_types::NedMutation,
    ) -> Response {
        conductor
            .handle_command(Command::CreateNedSuggestion {
                file: editorial_types::ContentPath::parse(file).expect("valid"),
                selection: selection.to_string(),
                mutation,
                reason: "test".to_string(),
                author: editorial_types::Author::Claude,
            })
            .response
    }

    fn create_error(
        conductor: &Conductor,
        file: &str,
        selection: &str,
        mutation: editorial_types::NedMutation,
    ) -> String {
        match create(conductor, file, selection, mutation) {
            Response::Error(e) => e,
            other => panic!("expected Error, got {other:?}"),
        }
    }

    fn no_suggestions_persisted(conductor: &Conductor, tmp: &tempfile::TempDir) {
        assert!(conductor.ned_suggestions.read().unwrap().is_empty());
        let dir = tmp.path().join(".presemble/suggestions/ned");
        // The `MODEL` marker (written at startup) is not a suggestion.
        let has_files = dir.exists()
            && std::fs::read_dir(&dir)
                .map(|d| d.flatten().any(|e| e.file_name() != "MODEL"))
                .unwrap_or(false);
        assert!(!has_files, "no suggestion file may be written");
    }

    #[test]
    fn text_in_title_selection_is_accepted() {
        let (conductor, _tmp) = make_conductor_with_doc();
        let src = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let resp = create(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("New".into()),
        );
        assert!(
            matches!(resp, Response::SuggestionCreated(_)),
            "got {resp:?}"
        );
    }

    /// Regression for the duplication bug: a slot address with SetText used to
    /// set every text under the slot.
    #[test]
    fn slot_address_with_set_text_is_rejected_with_text_in_hint() {
        let (conductor, tmp) = make_conductor_with_doc();
        let src = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#;
        let err = create_error(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("x".into()),
        );
        assert!(
            err.contains("ned/text-in"),
            "error should hint at ned/text-in: {err}"
        );
        no_suggestions_persisted(&conductor, &tmp);
    }

    #[test]
    fn block_address_with_set_text_is_rejected_with_text_in_hint() {
        let (conductor, _tmp) = make_conductor_with_doc();
        let src = r#"(ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title"))"#;
        let err = create_error(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("x".into()),
        );
        assert!(err.contains("heading"), "{err}");
        assert!(err.contains("ned/text-in"), "{err}");
    }

    #[test]
    fn multi_block_slot_is_rejected_as_ambiguous_with_hint() {
        let (conductor, tmp) = make_conductor_with_doc();
        let src = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "summary")))"#;
        let err = create_error(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("x".into()),
        );
        assert!(
            err.contains(&editorial_types::StaleCause::Ambiguous { matches: 2 }.to_string()),
            "{err}"
        );
        assert!(err.contains("ned/nth-of-kind"), "{err}");
        no_suggestions_persisted(&conductor, &tmp);
    }

    #[test]
    fn empty_selection_is_rejected_as_unresolved() {
        let (conductor, _tmp) = make_conductor_with_doc();
        let src = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/nope.md") "title")))"#;
        let err = create_error(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("x".into()),
        );
        assert!(
            err.contains(&editorial_types::StaleCause::Unresolved.to_string()),
            "{err}"
        );
        assert!(err.contains("Hint:"), "{err}");
    }

    #[test]
    fn malformed_clojure_is_rejected_as_eval_error() {
        let (conductor, _tmp) = make_conductor_with_doc();
        let err = create_error(
            &conductor,
            DOC,
            "(this is (not valid clojure",
            editorial_types::NedMutation::Delete,
        );
        assert!(err.contains("could not be evaluated"), "{err}");
    }

    /// Security regression: selections are evaluated query-only, so a
    /// selection that tries to mutate the store fails and changes nothing.
    #[test]
    fn mutating_selection_is_rejected_and_store_unchanged() {
        let (conductor, tmp) = make_conductor_with_doc();
        let src = r#"(do (ned/delete (ned/doc-by-path "content/test/doc.md")) (ned/doc-by-path "content/test/doc.md"))"#;
        let err = create_error(&conductor, DOC, src, editorial_types::NedMutation::Delete);
        assert!(err.contains("could not be evaluated"), "{err}");
        no_suggestions_persisted(&conductor, &tmp);

        let root = conductor
            .make_ned_root(&evaluator::ned_primitives::PlanRecorder::new())
            .expect("root");
        let count = evaluator::eval_str_with_root(
            r#"(ned/count (ned/doc-by-path "content/test/doc.md"))"#,
            &root,
        )
        .expect("count");
        assert!(
            matches!(count, template::Value::Integer(1)),
            "document must still exist exactly once, got {count:?}"
        );
    }

    #[test]
    fn file_mismatch_is_rejected() {
        let (conductor, tmp) = make_conductor_with_doc();
        let src = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let err = create_error(
            &conductor,
            "content/test/other.md",
            src,
            editorial_types::NedMutation::SetText("x".into()),
        );
        assert_eq!(
            err,
            "selection resolves into content/test/doc.md, not content/test/other.md"
        );
        no_suggestions_persisted(&conductor, &tmp);
    }

    #[test]
    fn invalid_value_is_rejected_at_creation() {
        let (conductor, _tmp) = make_conductor_with_doc();
        // A blank line would split a paragraph in two.
        let src = r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "summary")) "paragraph" 0))"#;
        let err = create_error(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("two\n\nparas".into()),
        );
        assert!(err.contains("invalid"), "{err}");
    }

    /// ADR-059 N5: a line break in a heading is a space, so the suggestion is
    /// valid (it was refused before headings held nodes).
    #[test]
    fn newline_in_a_heading_suggestion_is_valid() {
        let (conductor, _tmp) = make_conductor_with_doc();
        let src = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let resp = create(
            &conductor,
            DOC,
            src,
            editorial_types::NedMutation::SetText("two\nlines".into()),
        );
        assert!(
            matches!(resp, Response::SuggestionCreated(_)),
            "got {resp:?}"
        );
    }
}

#[cfg(test)]
mod ned_suggestion_handler_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    /// Build a minimal conductor with a real document in the NodeStore.
    ///
    /// Document at `content/test/doc.md` has:
    /// - "title" slot with a heading "Test Title" (SingleText)
    /// - "link-slot" slot with a link (NonText)
    /// - one body paragraph "Body text"
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::heading_from_markdown(
                            HeadingLevel::new(1).unwrap(),
                            "Test Title",
                        ),
                        span: Span { start: 0, end: 0 },
                    }],
                },
                DocumentSlot {
                    name: SlotName::new("link-slot"),
                    elements: im::vector![Spanned {
                        node: ContentElement::lone_link("Click me", "https://example.com"),
                        span: Span { start: 0, end: 0 },
                    }],
                },
            ],
            body: im::vector![Spanned {
                node: ContentElement::paragraph_from_markdown("Body text"),
                span: Span { start: 0, end: 0 },
            },],
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };

        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: "content/test/doc.md".to_string(),
            page_kind: "item".to_string(),
        };
        {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }

        (conductor, tmp)
    }

    // ── 1. Query-only evaluation root ─────────────────────────────────────────

    #[test]
    fn query_root_evaluates_selections_but_binds_no_mutations() {
        let (conductor, _tmp) = make_conductor_with_doc();
        let root = conductor
            .make_ned_query_root()
            .expect("make_ned_query_root");

        let selection = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let value = evaluator::eval_str_with_root(selection, &root).expect("query evaluates");
        let sel = evaluator::ned_primitives::extract_selection(&value).expect("selection");
        assert_eq!(sel.len(), 1, "title text resolves to exactly one node");

        for name in evaluator::ned_primitives::NED_MUTATING_BUILTINS {
            assert!(
                evaluator::eval_str_with_root(name, &root).is_err(),
                "{name} must not be bound in the query root"
            );
        }
    }

    // ── 2. Create happy path ──────────────────────────────────────────────────

    #[test]
    fn create_ned_suggestion_happy_path() {
        let (conductor, tmp) = make_conductor_with_doc();

        let selection = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let cmd = Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Updated Title".to_string()),
            reason: "Better title".to_string(),
            author: editorial_types::Author::Claude,
        };

        let result = conductor.handle_command(cmd);

        // Should return SuggestionCreated
        let id = match result.response {
            Response::SuggestionCreated(ref id) => id.clone(),
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Should emit NedSuggestionCreated event
        assert!(
            result
                .events
                .iter()
                .any(|ev| matches!(ev, ConductorEvent::NedSuggestionCreated { .. })),
            "should emit NedSuggestionCreated event"
        );

        // Should be in memory with Pending status
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion should be in memory");
        assert_eq!(sug.status, editorial_types::NedSuggestionStatus::Pending);
        drop(map);

        // Should be persisted to disk
        let file_path = tmp
            .path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        assert!(
            file_path.exists(),
            "suggestion file should exist at {}",
            file_path.display()
        );

        // File should deserialize cleanly
        let contents = std::fs::read_to_string(&file_path).expect("read file");
        let deserialized: editorial_types::NedSuggestion =
            serde_json::from_str(&contents).expect("deserialize");
        assert_eq!(deserialized.id, id);
        assert_eq!(
            deserialized.status,
            editorial_types::NedSuggestionStatus::Pending
        );
    }

    // ── 3. Create rejection (non-text slot) ───────────────────────────────────

    #[test]
    fn create_ned_suggestion_rejects_non_text_slot() {
        let (conductor, tmp) = make_conductor_with_doc();

        // link-slot is NonText — should be rejected at creation
        let selection = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "link-slot")"#;
        let cmd = Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("x".to_string()),
            reason: "should fail".to_string(),
            author: editorial_types::Author::Claude,
        };

        let result = conductor.handle_command(cmd);

        // Should return an error
        assert!(
            matches!(result.response, Response::Error(_)),
            "expected Response::Error, got: {:?}",
            result.response
        );

        // Nothing should be written to disk
        let ned_dir = tmp.path().join(".presemble/suggestions/ned");
        // (The `MODEL` marker written at startup is not a suggestion.)
        let has_files = ned_dir.exists()
            && std::fs::read_dir(&ned_dir)
                .map(|d| d.flatten().any(|e| e.file_name() != "MODEL"))
                .unwrap_or(false);
        assert!(
            !has_files,
            "no suggestion files should exist after rejection"
        );

        // Nothing in memory
        let map = conductor.ned_suggestions.read().unwrap();
        assert!(
            map.is_empty(),
            "ned_suggestions map should be empty after rejection"
        );
    }

    // ── 4. Accept happy path ──────────────────────────────────────────────────

    #[test]
    fn accept_ned_suggestion_happy_path() {
        let (conductor, tmp) = make_conductor_with_doc();

        let selection = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SearchReplace {
                search: "Test Title".to_string(),
                replace: "Accepted Title".to_string(),
            },
            reason: "clearer title".to_string(),
            author: editorial_types::Author::Claude,
        };

        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Now accept
        let accept_cmd = Command::AcceptNedSuggestion { id: id.clone() };
        let accept_result = conductor.handle_command(accept_cmd);

        assert!(
            matches!(accept_result.response, Response::Ok),
            "expected Ok on accept, got: {:?}",
            accept_result.response
        );

        // AcceptNedSuggestion should emit NedSuggestionAccepted event
        assert!(
            accept_result
                .events
                .iter()
                .any(|ev| matches!(ev, ConductorEvent::NedSuggestionAccepted { .. })),
            "should emit NedSuggestionAccepted event; got: {:?}",
            accept_result.events
        );

        // Status in memory should be Accepted
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion in memory");
        assert_eq!(sug.status, editorial_types::NedSuggestionStatus::Accepted);
        drop(map);

        // File should be updated on disk
        let file_path = tmp
            .path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        let contents = std::fs::read_to_string(&file_path).expect("read file");
        let deserialized: editorial_types::NedSuggestion =
            serde_json::from_str(&contents).expect("deserialize");
        assert_eq!(
            deserialized.status,
            editorial_types::NedSuggestionStatus::Accepted
        );
    }

    // ── 5. Accept on broken selection (stale) ─────────────────────────────────

    #[test]
    fn accept_ned_suggestion_stale_on_broken_selection() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Create a suggestion targeting a document that doesn't actually exist on disk
        // so that after we clear the NodeStore, the selection will fail to resolve.
        let selection = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Changed".to_string()),
            reason: "test stale".to_string(),
            author: editorial_types::Author::Claude,
        };

        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Mutate the store to remove the document so the selection yields empty
        {
            let mut store = conductor.node_store.write().unwrap();
            store.clear();
        }

        // Now accept — should go Stale (empty selection)
        let accept_cmd = Command::AcceptNedSuggestion { id: id.clone() };
        let accept_result = conductor.handle_command(accept_cmd);

        // Returns Ok (not an error — lifecycle did its job)
        assert!(
            matches!(accept_result.response, Response::Ok),
            "expected Ok (stale path), got: {:?}",
            accept_result.response
        );

        // Should emit NedSuggestionStaled event
        assert!(
            accept_result
                .events
                .iter()
                .any(|ev| matches!(ev, ConductorEvent::NedSuggestionStaled { .. })),
            "should emit NedSuggestionStaled event; got: {:?}",
            accept_result.events
        );

        // Status should be Stale with cause Unresolved
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion in memory");
        assert!(
            matches!(
                sug.status,
                editorial_types::NedSuggestionStatus::Stale {
                    cause: editorial_types::StaleCause::Unresolved,
                    ..
                }
            ),
            "expected Stale(Unresolved) status, got: {:?}",
            sug.status
        );
    }

    // ── 6. Reject ────────────────────────────────────────────────────────────

    #[test]
    fn reject_ned_suggestion() {
        let (conductor, tmp) = make_conductor_with_doc();

        // Delete addresses a block: the title slot's single heading.
        let selection =
            r#"(ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title"))"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::Delete,
            reason: "test reject".to_string(),
            author: editorial_types::Author::Human("Alice".to_string()),
        };

        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        let reject_cmd = Command::RejectNedSuggestion { id: id.clone() };
        let reject_result = conductor.handle_command(reject_cmd);

        assert!(
            matches!(reject_result.response, Response::Ok),
            "expected Ok on reject, got: {:?}",
            reject_result.response
        );

        // RejectNedSuggestion should emit NedSuggestionRejected event
        assert!(
            reject_result
                .events
                .iter()
                .any(|ev| matches!(ev, ConductorEvent::NedSuggestionRejected { .. })),
            "should emit NedSuggestionRejected event; got: {:?}",
            reject_result.events
        );

        // Status in memory should be Rejected
        let map = conductor.ned_suggestions.read().unwrap();
        let sug = map.get(&id).expect("suggestion in memory");
        assert_eq!(sug.status, editorial_types::NedSuggestionStatus::Rejected);
        drop(map);

        // File on disk should be updated
        let file_path = tmp
            .path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        let contents = std::fs::read_to_string(&file_path).expect("read file");
        let deserialized: editorial_types::NedSuggestion =
            serde_json::from_str(&contents).expect("deserialize");
        assert_eq!(
            deserialized.status,
            editorial_types::NedSuggestionStatus::Rejected
        );
    }

    // ── GetNedSuggestions tests ────────────────────────────────────────────────

    #[test]
    fn get_ned_suggestions_empty_for_unknown_file() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let file =
            editorial_types::ContentPath::parse("content/test/nonexistent.md").expect("valid");
        let result = conductor.handle_command(Command::GetNedSuggestions { file });

        match result.response {
            Response::NedSuggestions(v) => assert!(v.is_empty(), "expected empty vec"),
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestions_returns_pending_suggestion() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let selection = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
        let create_cmd = Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Pending Title".to_string()),
            reason: "pending test".to_string(),
            author: editorial_types::Author::Claude,
        };
        let create_result = conductor.handle_command(create_cmd);
        let id = match create_result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        let file = editorial_types::ContentPath::parse("content/test/doc.md").expect("valid");
        let result = conductor.handle_command(Command::GetNedSuggestions { file });

        match result.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 1, "expected one suggestion");
                assert_eq!(v[0].id, id);
                assert_eq!(v[0].status, editorial_types::NedSuggestionStatus::Pending);
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestions_returns_all_statuses() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let file = editorial_types::ContentPath::parse("content/test/doc.md").expect("valid");
        let file_path = file.clone();
        let selection = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;

        // Create a Pending suggestion
        let create_pending = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Pending".to_string()),
            reason: "pending".to_string(),
            author: editorial_types::Author::Claude,
        };
        conductor.handle_command(create_pending);

        // Create and accept a suggestion → Accepted
        let create_accepted = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Accepted".to_string()),
            reason: "to accept".to_string(),
            author: editorial_types::Author::Claude,
        };
        let accepted_id = match conductor.handle_command(create_accepted).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };
        conductor.handle_command(Command::AcceptNedSuggestion { id: accepted_id });

        // Create and reject a suggestion → Rejected
        let create_rejected = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Rejected".to_string()),
            reason: "to reject".to_string(),
            author: editorial_types::Author::Claude,
        };
        let rejected_id = match conductor.handle_command(create_rejected).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };
        conductor.handle_command(Command::RejectNedSuggestion { id: rejected_id });

        // Create a suggestion that goes Stale: clear the NodeStore so the
        // selection fails to resolve on accept.
        let create_stale = Command::CreateNedSuggestion {
            file: file_path.clone(),
            selection: selection.to_string(),
            mutation: editorial_types::NedMutation::SetText("Stale".to_string()),
            reason: "to go stale".to_string(),
            author: editorial_types::Author::Claude,
        };
        let stale_id = match conductor.handle_command(create_stale).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };
        // Wipe the NodeStore so selection resolution fails → Stale
        conductor.node_store.write().unwrap().clear();
        conductor.handle_command(Command::AcceptNedSuggestion { id: stale_id });

        // Query all suggestions for the file
        let result = conductor.handle_command(Command::GetNedSuggestions { file });

        match result.response {
            Response::NedSuggestions(v) => {
                assert_eq!(
                    v.len(),
                    4,
                    "expected four suggestions (Pending, Accepted, Rejected, Stale)"
                );
                let statuses: Vec<_> = v.iter().map(|s| &s.status).collect();
                assert!(
                    statuses
                        .iter()
                        .any(|s| **s == editorial_types::NedSuggestionStatus::Pending),
                    "missing Pending"
                );
                assert!(
                    statuses
                        .iter()
                        .any(|s| **s == editorial_types::NedSuggestionStatus::Accepted),
                    "missing Accepted"
                );
                assert!(
                    statuses
                        .iter()
                        .any(|s| **s == editorial_types::NedSuggestionStatus::Rejected),
                    "missing Rejected"
                );
                assert!(
                    statuses
                        .iter()
                        .any(|s| matches!(s, editorial_types::NedSuggestionStatus::Stale { .. })),
                    "missing Stale"
                );
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestions_filters_by_file() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let file_a = editorial_types::ContentPath::parse("content/test/doc.md").expect("valid");
        let selection_a = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;

        // Create suggestion on file_a
        let create_a = Command::CreateNedSuggestion {
            file: file_a.clone(),
            selection: selection_a.to_string(),
            mutation: editorial_types::NedMutation::SetText("For file A".to_string()),
            reason: "file a".to_string(),
            author: editorial_types::Author::Claude,
        };
        let id_a = match conductor.handle_command(create_a).response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got: {other:?}"),
        };

        // Insert a suggestion for file_b directly into the map (no doc in NodeStore)
        let id_b = editorial_types::SuggestionId::new();
        let sug_b = editorial_types::NedSuggestion {
            id: id_b.clone(),
            author: editorial_types::Author::Human("editor".to_string()),
            file: editorial_types::ContentPath::parse("content/test/other.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("For file B".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file b".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-26T00:00:00Z".to_string(),
            preview: None,
        };
        conductor
            .ned_suggestions
            .write()
            .unwrap()
            .insert(id_b, sug_b);

        // Query file_a only
        let file_a_query =
            editorial_types::ContentPath::parse("content/test/doc.md").expect("valid");
        let result_a = conductor.handle_command(Command::GetNedSuggestions { file: file_a_query });

        match result_a.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 1, "expected only file_a suggestion");
                assert_eq!(v[0].id, id_a);
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }

        // Query file_b only
        let file_b_query =
            editorial_types::ContentPath::parse("content/test/other.md").expect("valid");
        let result_b = conductor.handle_command(Command::GetNedSuggestions { file: file_b_query });

        match result_b.response {
            Response::NedSuggestions(v) => {
                assert_eq!(v.len(), 1, "expected only file_b suggestion");
                assert_eq!(
                    v[0].file,
                    editorial_types::ContentPath::parse("content/test/other.md").expect("valid")
                );
            }
            other => panic!("expected NedSuggestions, got: {other:?}"),
        }
    }

    // ── GetNedSuggestionFiles tests ───────────────────────────────────────────

    #[test]
    fn get_ned_suggestion_files_returns_only_pending_files() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let id_a = editorial_types::SuggestionId::new();
        let sug_a = editorial_types::NedSuggestion {
            id: id_a.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/alpha.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Alpha".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file alpha".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };

        let id_b = editorial_types::SuggestionId::new();
        let sug_b = editorial_types::NedSuggestion {
            id: id_b.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/beta.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Beta".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file beta".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };

        // Insert a Rejected suggestion for a third file -- must NOT appear in results.
        let id_c = editorial_types::SuggestionId::new();
        let sug_c = editorial_types::NedSuggestion {
            id: id_c.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/gamma.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Gamma".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "file gamma rejected".to_string(),
            status: editorial_types::NedSuggestionStatus::Rejected,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };

        {
            let mut map = conductor.ned_suggestions.write().unwrap();
            map.insert(id_a, sug_a);
            map.insert(id_b, sug_b);
            map.insert(id_c, sug_c);
        }

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert_eq!(files.len(), 2, "expected exactly two pending files");
                assert_eq!(files[0], "content/test/alpha.md");
                assert_eq!(files[1], "content/test/beta.md");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestion_files_empty_when_only_rejected_or_accepted() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let id_rejected = editorial_types::SuggestionId::new();
        let sug_rejected = editorial_types::NedSuggestion {
            id: id_rejected.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("No show".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "rejected".to_string(),
            status: editorial_types::NedSuggestionStatus::Rejected,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };

        let id_accepted = editorial_types::SuggestionId::new();
        let sug_accepted = editorial_types::NedSuggestion {
            id: id_accepted.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/other.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Accepted".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "accepted".to_string(),
            status: editorial_types::NedSuggestionStatus::Accepted,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };

        {
            let mut map = conductor.ned_suggestions.write().unwrap();
            map.insert(id_rejected, sug_rejected);
            map.insert(id_accepted, sug_accepted);
        }

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert!(
                    files.is_empty(),
                    "expected no files when only Rejected and Accepted"
                );
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestion_files_includes_stale_files() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // One Stale suggestion — should appear in the sidebar so the user can dismiss it.
        let id_stale = editorial_types::SuggestionId::new();
        let sug_stale = editorial_types::NedSuggestion {
            id: id_stale.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/stale-doc.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Stale".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "stale reason".to_string(),
            status: editorial_types::NedSuggestionStatus::Stale {
                reason: "selection no longer resolves".to_string(),
                cause: editorial_types::StaleCause::Unresolved,
            },
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };
        conductor
            .ned_suggestions
            .write()
            .unwrap()
            .insert(id_stale, sug_stale);

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert_eq!(files.len(), 1, "stale file must appear in sidebar");
                assert_eq!(files[0], "content/test/stale-doc.md");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }

    #[test]
    fn get_ned_suggestion_files_deduplicates_same_file() {
        let (conductor, _tmp) = make_conductor_with_doc();

        let id_1 = editorial_types::SuggestionId::new();
        let sug_1 = editorial_types::NedSuggestion {
            id: id_1.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("First".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "first".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };
        let id_2 = editorial_types::SuggestionId::new();
        let sug_2 = editorial_types::NedSuggestion {
            id: id_2.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse("content/test/doc.md").expect("valid"),
            selection: "(some-selection)".to_string(),
            mutation: editorial_types::NedMutation::SetText("Second".to_string()),
            workspace_hash: "abc".to_string(),
            reason: "second".to_string(),
            status: editorial_types::NedSuggestionStatus::Pending,
            created_at: "2026-04-28T00:00:00Z".to_string(),
            preview: None,
        };
        {
            let mut map = conductor.ned_suggestions.write().unwrap();
            map.insert(id_1, sug_1);
            map.insert(id_2, sug_2);
        }

        let result = conductor.handle_command(Command::GetNedSuggestionFiles);

        match result.response {
            Response::NedSuggestionFiles(files) => {
                assert_eq!(files.len(), 1, "expected deduplicated to one file");
                assert_eq!(files[0], "content/test/doc.md");
            }
            other => panic!("expected NedSuggestionFiles, got: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// ADR-056 accept: resolve + validate + apply under one write lock, against a
// real on-disk site so pages actually re-render.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod suggestion_accept_tests {
    use super::*;
    use editorial_types::{NedMutation, NedSuggestionStatus, StaleCause, SuggestionId};

    const FILE: &str = "content/post/first.md";
    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert></body></html>"#;
    const CONTENT: &str =
        "# First Post\n\n----\n\nAlpha para.\n\nBeta para.\n\n```rust\nfn a() {}\n```\n";

    const TITLE_TEXT: &str = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "title")))"#;
    const FIRST_PARAGRAPH: &str = r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "paragraph" 0)"#;
    const ALPHA_TEXT: &str =
        r#"(ned/text-containing (ned/body (ned/doc-by-path "content/post/first.md")) "Alpha")"#;
    const CODE_TEXT: &str = r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "code-block" 0))"#;

    fn build_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("schemas/post")).unwrap();
        std::fs::create_dir_all(root.join("templates/post")).unwrap();
        std::fs::create_dir_all(root.join("content/post")).unwrap();
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), CONTENT).unwrap();
        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    fn create(conductor: &Conductor, selection: &str, mutation: NedMutation) -> SuggestionId {
        let result = conductor.handle_command(Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse(FILE).expect("valid"),
            selection: selection.to_string(),
            mutation,
            reason: "test".to_string(),
            author: editorial_types::Author::Claude,
        });
        match result.response {
            Response::SuggestionCreated(id) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        }
    }

    fn accept(conductor: &Conductor, id: &SuggestionId) -> CommandResult {
        conductor.handle_command(Command::AcceptNedSuggestion { id: id.clone() })
    }

    fn status(conductor: &Conductor, id: &SuggestionId) -> NedSuggestionStatus {
        conductor
            .ned_suggestions
            .read()
            .unwrap()
            .get(id)
            .expect("suggestion")
            .status
            .clone()
    }

    fn live_root(conductor: &Conductor) -> node_store::NodeId {
        conductor
            .doc_root_for_path(Path::new(FILE))
            .expect("live root")
    }

    fn block_text(store: &node_store::NodeStore, block: node_store::NodeId) -> String {
        match store.children(block).first().and_then(|t| store.get(*t)) {
            Some(node_store::Node::Text(s)) => s.clone(),
            other => panic!("expected a text child, got {other:?}"),
        }
    }

    fn body_texts(conductor: &Conductor) -> Vec<String> {
        let root = live_root(conductor);
        let store = conductor.node_store.read().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&store, root, "body")
            .expect("body");
        store
            .children(body)
            .into_iter()
            .map(|b| block_text(&store, b))
            .collect()
    }

    fn title(conductor: &Conductor) -> String {
        let root = live_root(conductor);
        let store = conductor.node_store.read().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                .expect("preamble");
        let slot = store.children(preamble)[0];
        block_text(&store, store.children(slot)[0])
    }

    fn is_dirty(conductor: &Conductor) -> bool {
        conductor
            .dirty_docs()
            .read()
            .unwrap()
            .contains(Path::new(FILE))
    }

    /// Delete the document's first body block behind the suggestion's back,
    /// through the same typed operations an accepted suggestion uses.
    fn delete_first_body_block(conductor: &Conductor) {
        let root = live_root(conductor);
        let mut store = conductor.node_store.write().unwrap();
        let body =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "body").unwrap();
        let first = store.children(body)[0];
        let target = edit::resolve(
            &store,
            &ned::Selection::single(first),
            edit::TargetKind::Block,
        )
        .expect("the block resolves");
        let prepared =
            edit::prepare(&store, &target, &edit::NedMutation::Delete).expect("delete prepares");
        edit::commit(&mut store, vec![prepared]);
    }

    fn accepted_pages(result: &CommandResult) -> Vec<String> {
        result
            .events
            .iter()
            .find_map(|ev| match ev {
                ConductorEvent::NedSuggestionAccepted { pages, .. } => Some(pages.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no NedSuggestionAccepted in {:?}", result.events))
    }

    fn original_body() -> Vec<String> {
        vec![
            "Alpha para.".to_string(),
            "Beta para.".to_string(),
            "fn a() {}".to_string(),
        ]
    }

    #[test]
    fn fixture_has_expected_shape() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(title(&conductor), "First Post");
        assert_eq!(body_texts(&conductor), original_body());
        assert!(!is_dirty(&conductor));
    }

    #[test]
    fn accept_set_text_on_title_changes_only_title_and_rebuilds() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            TITLE_TEXT,
            NedMutation::SetText("Renamed Post".into()),
        );

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert_eq!(title(&conductor), "Renamed Post");
        assert_eq!(
            body_texts(&conductor),
            original_body(),
            "body must be untouched"
        );
        assert!(is_dirty(&conductor), "file must be marked dirty");
        let pages = accepted_pages(&result);
        assert!(!pages.is_empty(), "accepted event must list rebuilt pages");
        assert!(
            pages.contains(&"/post/first".to_string()),
            "pages: {pages:?}"
        );
    }

    #[test]
    fn accept_of_a_set_text_suggestion_already_in_place_is_accepted() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            TITLE_TEXT,
            NedMutation::SetText("Renamed Post".into()),
        );

        // The title becomes "Renamed Post" another way (no preview, so the
        // only way to know is that the edit would change nothing).
        {
            let root = live_root(&conductor);
            let mut store = conductor.node_store.write().unwrap();
            let preamble =
                node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                    .unwrap();
            let slot = store.children(preamble)[0];
            let heading = store.children(slot)[0];
            let text = store.children(heading)[0];
            store.replace_node(text, node_store::Node::Text("Renamed Post".into()));
        }

        let result = accept(&conductor, &id);

        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert!(accepted_pages(&result).is_empty(), "nothing is rebuilt");
        assert!(
            !is_dirty(&conductor),
            "nothing changed, so nothing is dirty"
        );
        assert_eq!(title(&conductor), "Renamed Post");
    }

    /// A selection that resolves into another file never counts as in place,
    /// even when that block happens to read as the proposal: the file check
    /// comes first.
    #[test]
    fn accept_never_counts_a_block_in_another_file_as_in_place() {
        use content::{ContentElement, Document, DocumentSlot};
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
        use schema::{HeadingLevel, SlotName, Span, Spanned};
        const OTHER: &str = "content/post/second.md";

        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // B: a second live document whose title reads "Same".
        {
            let doc = Document {
                preamble: im::vector![DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::heading_from_markdown(
                            HeadingLevel::new(1).unwrap(),
                            "Same",
                        ),
                        span: Span { start: 0, end: 0 },
                    }],
                }],
                body: im::vector![],
                has_separator: false,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
            };
            let meta = DocumentMeta {
                url: "/post/second".to_string(),
                stem: "post".to_string(),
                file: OTHER.to_string(),
                page_kind: "item".to_string(),
            };
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }
        // A suggestion for FILE whose selection addresses B's title.
        let id = SuggestionId::new();
        let sug = editorial_types::NedSuggestion {
            id: id.clone(),
            author: editorial_types::Author::Claude,
            file: editorial_types::ContentPath::parse(FILE).expect("valid"),
            selection: format!(
                r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "{OTHER}") "title")))"#
            ),
            mutation: NedMutation::SetText("Same".into()),
            workspace_hash: "abc".to_string(),
            reason: "test".to_string(),
            status: NedSuggestionStatus::Pending,
            created_at: "2026-10-07T00:00:00Z".to_string(),
            preview: Some(editorial_types::SuggestionPreview {
                base: editorial_types::PreviewText::new("Same"),
                proposed: editorial_types::PreviewText::new("Same"),
            }),
        };
        conductor
            .ned_suggestions
            .write()
            .unwrap()
            .insert(id.clone(), sug);

        let result = accept(&conductor, &id);

        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert!(
            matches!(
                status(&conductor, &id),
                NedSuggestionStatus::Stale {
                    cause: StaleCause::Unresolved,
                    ..
                }
            ),
            "{:?}",
            status(&conductor, &id)
        );
        assert!(!is_dirty(&conductor));
    }

    #[test]
    fn creating_a_no_op_suggestion_answers_already_in_place() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);

        // The title already reads "First Post".
        let result = conductor.handle_command(Command::CreateNedSuggestion {
            file: editorial_types::ContentPath::parse(FILE).expect("valid"),
            selection: TITLE_TEXT.to_string(),
            mutation: NedMutation::SetText("First Post".into()),
            reason: "test".to_string(),
            author: editorial_types::Author::Claude,
        });

        assert!(
            matches!(result.response, Response::AlreadyInPlace),
            "got {:?}",
            result.response
        );
        assert!(result.events.is_empty(), "{:?}", result.events);
        assert!(
            conductor.ned_suggestions.read().unwrap().is_empty(),
            "nothing is stored"
        );
    }

    /// An external change to one file repopulates the store, but must not
    /// throw away unsaved edits in the other documents.
    #[test]
    fn external_change_to_another_file_keeps_unsaved_edits() {
        const OTHER: &str = "content/post/second.md";
        let tmp = build_site();
        std::fs::write(tmp.path().join(OTHER), "# Second Post\n\n----\n\nGamma.\n").unwrap();
        let conductor = make_conductor(&tmp);

        // An unsaved edit to A.
        let program = format!(r#"(ned/set-text {TITLE_TEXT} "Edited A")"#);
        assert!(matches!(
            conductor.apply_ned_program(&program).response,
            Response::Applied { dirty_paths: 1, .. }
        ));
        assert_eq!(title(&conductor), "Edited A");
        assert!(is_dirty(&conductor));

        // B changes on disk, from outside.
        std::fs::write(tmp.path().join(OTHER), "# Second Post\n\n----\n\nDelta.\n").unwrap();
        let result = conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(OTHER).expect("valid")],
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );

        assert_eq!(title(&conductor), "Edited A", "A's unsaved edit survives");
        assert!(is_dirty(&conductor), "A is still dirty");
        let dirty = match conductor.handle_command(Command::GetDirtyBuffers).response {
            Response::DirtyBuffers(paths) => paths,
            other => panic!("expected DirtyBuffers, got {other:?}"),
        };
        assert_eq!(dirty, vec![FILE.to_string()]);
    }

    fn title_of(conductor: &Conductor, file: &str) -> String {
        let root = conductor
            .doc_root_for_path(Path::new(file))
            .expect("live root");
        let store = conductor.node_store.read().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                .expect("preamble");
        let slot = store.children(preamble)[0];
        block_text(&store, store.children(slot)[0])
    }

    fn edit_title(conductor: &Conductor, file: &str, text: &str) {
        let program = format!(
            r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{file}") "title"))) "{text}")"#
        );
        assert!(matches!(
            conductor.apply_ned_program(&program).response,
            Response::Applied { dirty_paths: 1, .. }
        ));
    }

    fn reload(conductor: &Conductor, file: &str) -> CommandResult {
        conductor.handle_command(Command::ReloadDocument {
            path: editorial_types::ContentPath::parse(file).expect("valid"),
        })
    }

    #[test]
    fn reload_discards_unsaved_edits_and_says_so() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        assert!(is_dirty(&conductor));

        let result = reload(&conductor, FILE);

        assert!(
            matches!(result.response, Response::Reloaded { discarded: true }),
            "{:?}",
            result.response
        );
        assert_eq!(title(&conductor), "First Post", "the disk text is back");
        assert!(!is_dirty(&conductor), "no longer dirty");
        assert!(
            result.events.iter().any(|e| matches!(e, ConductorEvent::PagesRebuilt { pages, .. } if pages.contains(&"/post/first".to_string()))),
            "{:?}",
            result.events
        );
    }

    #[test]
    fn reload_of_a_clean_document_reports_nothing_discarded() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let result = reload(&conductor, FILE);
        assert!(
            matches!(result.response, Response::Reloaded { discarded: false }),
            "{:?}",
            result.response
        );
        assert_eq!(title(&conductor), "First Post");
    }

    #[test]
    fn reload_keeps_other_documents_unsaved_edits() {
        const OTHER: &str = "content/post/second.md";
        let tmp = build_site();
        std::fs::write(tmp.path().join(OTHER), "# Second Post\n\n----\n\nGamma.\n").unwrap();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        edit_title(&conductor, OTHER, "Edited B");

        let result = reload(&conductor, FILE);

        assert!(
            matches!(result.response, Response::Reloaded { discarded: true }),
            "{:?}",
            result.response
        );
        assert_eq!(title_of(&conductor, FILE), "First Post");
        assert_eq!(
            title_of(&conductor, OTHER),
            "Edited B",
            "B's unsaved edit survives"
        );
        let dirty = match conductor.handle_command(Command::GetDirtyBuffers).response {
            Response::DirtyBuffers(paths) => paths,
            other => panic!("expected DirtyBuffers, got {other:?}"),
        };
        assert_eq!(dirty, vec![OTHER.to_string()]);
    }

    // ── Disk conflicts (ADR-065) ────────────────────────────────────────────

    const OTHER_FILE: &str = "content/post/second.md";
    const SHELL_A: &str = "# Shell Title\n\n----\n\nShell.\n";
    const SHELL_B: &str = "# Shell B\n\n----\n\nShell B.\n";

    /// A and B on disk; unsaved edits in both ("Edited A", "Edited B"); then
    /// both files are overwritten from outside and the watcher reports it.
    /// With `both == false` only A is edited and overwritten.
    fn conflicted(both: bool) -> (Conductor, tempfile::TempDir) {
        let tmp = build_site();
        std::fs::write(
            tmp.path().join(OTHER_FILE),
            "# Second Post\n\n----\n\nGamma.\n",
        )
        .unwrap();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        std::fs::write(tmp.path().join(FILE), SHELL_A).unwrap();
        let mut changed = vec![editorial_types::SitePath::parse(FILE).expect("valid")];
        if both {
            edit_title(&conductor, OTHER_FILE, "Edited B");
            std::fs::write(tmp.path().join(OTHER_FILE), SHELL_B).unwrap();
            changed.push(editorial_types::SitePath::parse(OTHER_FILE).expect("valid"));
        }
        conductor.handle_command(Command::FileChanged { paths: changed });
        (conductor, tmp)
    }

    fn conflicts(conductor: &Conductor) -> Vec<String> {
        match conductor.handle_command(Command::GetConflicts).response {
            Response::Conflicts(files) => files.into_iter().map(|c| c.file).collect(),
            other => panic!("expected Conflicts, got {other:?}"),
        }
    }

    fn consent(files: &[&str]) -> editorial_types::ConflictConsent {
        editorial_types::ConflictConsent::Overwrite(files.iter().map(|f| f.to_string()).collect())
    }

    fn disk(tmp: &tempfile::TempDir, file: &str) -> String {
        std::fs::read_to_string(tmp.path().join(file)).unwrap()
    }

    #[test]
    fn an_unchanged_conflict_is_not_announced_again() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        std::fs::write(tmp.path().join(FILE), SHELL_A).unwrap();

        let result = conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(FILE).expect("valid")],
        });

        assert_eq!(
            title(&conductor),
            "Edited A",
            "the disk version does not win"
        );
        assert!(is_dirty(&conductor));
        assert_eq!(conflicts(&conductor), vec![FILE.to_string()]);
        let announced = result
            .events
            .iter()
            .filter(|e| matches!(e, ConductorEvent::DiskConflict { file, .. } if file == FILE))
            .count();
        assert_eq!(announced, 1, "{:?}", result.events);

        // A second disk change that leaves the conflict as it was (the body
        // changes, and is merged) is not announced again.
        std::fs::write(tmp.path().join(FILE), "# Shell Title\n\n----\n\nAgain.\n").unwrap();
        let again = conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(FILE).expect("valid")],
        });
        assert!(
            !again
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::DiskConflict { .. })),
            "{:?}",
            again.events
        );
    }

    #[test]
    fn editor_save_over_a_dirty_document_is_a_conflict() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");

        // The editor wrote the file itself, then says so.
        std::fs::write(tmp.path().join(FILE), SHELL_A).unwrap();
        let result = conductor.handle_command(Command::DocumentSaved {
            path: editorial_types::SitePath::parse(FILE).expect("valid"),
        });

        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::DiskConflict { file, .. } if file == FILE)),
            "{:?}",
            result.events
        );
        assert_eq!(conflicts(&conductor), vec![FILE.to_string()]);
        assert_eq!(title(&conductor), "Edited A");
    }

    #[test]
    fn save_all_with_a_conflict_asks_for_confirmation_and_writes_nothing() {
        let (conductor, tmp) = conflicted(true);

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: Default::default(),
            })
            .response;

        match response {
            Response::ConflictsNeedConfirmation(files) => {
                assert_eq!(files, vec![FILE.to_string(), OTHER_FILE.to_string()]);
            }
            other => panic!("expected ConflictsNeedConfirmation, got {other:?}"),
        }
        assert_eq!(disk(&tmp, FILE), SHELL_A, "nothing was written");
        assert_eq!(disk(&tmp, OTHER_FILE), SHELL_B, "nothing was written");
        assert_eq!(
            conflicts(&conductor),
            vec![FILE.to_string(), OTHER_FILE.to_string()]
        );
    }

    #[test]
    fn save_all_with_consent_overwrites_the_listed_conflicts_and_clears_them() {
        let (conductor, tmp) = conflicted(true);

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: consent(&[FILE, OTHER_FILE]),
            })
            .response;

        assert!(matches!(response, Response::Ok), "{response:?}");
        assert!(
            disk(&tmp, FILE).contains("Edited A"),
            "{}",
            disk(&tmp, FILE)
        );
        assert!(disk(&tmp, OTHER_FILE).contains("Edited B"));
        assert!(conflicts(&conductor).is_empty());
        assert!(!is_dirty(&conductor));
    }

    #[test]
    fn consent_does_not_cover_a_conflict_that_appeared_later() {
        // The user was warned about A only; B conflicted afterwards.
        let (conductor, tmp) = conflicted(true);

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: consent(&[FILE]),
            })
            .response;

        match response {
            Response::ConflictsNeedConfirmation(files) => {
                assert_eq!(
                    files,
                    vec![OTHER_FILE.to_string()],
                    "only the uncovered conflict"
                );
            }
            other => panic!("expected ConflictsNeedConfirmation, got {other:?}"),
        }
        assert_eq!(
            disk(&tmp, FILE),
            SHELL_A,
            "nothing is written, not even the covered file"
        );
        assert_eq!(disk(&tmp, OTHER_FILE), SHELL_B);
    }

    #[test]
    fn save_buffer_on_a_conflict_needs_consent() {
        let (conductor, tmp) = conflicted(false);

        let refused = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            })
            .response;
        assert!(
            matches!(&refused, Response::ConflictsNeedConfirmation(files) if files == &vec![FILE.to_string()]),
            "{refused:?}"
        );
        assert_eq!(disk(&tmp, FILE), SHELL_A);

        let saved = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: consent(&[FILE]),
            })
            .response;
        assert!(matches!(saved, Response::Ok), "{saved:?}");
        assert!(disk(&tmp, FILE).contains("Edited A"));
        assert!(conflicts(&conductor).is_empty());
    }

    // ── A save announces what it wrote (the published output follows saves) ──

    fn saved_files(events: &[ConductorEvent]) -> Vec<String> {
        let mut files: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                ConductorEvent::DocumentsSaved { files } => Some(files.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        files.sort();
        files
    }

    #[test]
    fn save_all_announces_the_documents_it_wrote() {
        let (conductor, _tmp) = conflicted(true);
        let result = conductor.handle_command(Command::SaveAllBuffers {
            consent: consent(&[FILE, OTHER_FILE]),
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(
            saved_files(&result.events),
            vec![FILE.to_string(), OTHER_FILE.to_string()]
        );
    }

    #[test]
    fn save_buffer_announces_the_document_it_wrote() {
        let (conductor, _tmp) = conflicted(false);
        let result = conductor.handle_command(Command::SaveBuffer {
            path: editorial_types::SitePath::parse(FILE).expect("valid"),
            consent: consent(&[FILE]),
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(saved_files(&result.events), vec![FILE.to_string()]);
    }

    /// GUARD: a save that is refused wrote nothing and announces nothing saved.
    #[test]
    fn a_refused_save_announces_nothing_saved() {
        let (conductor, _tmp) = conflicted(true);
        let result = conductor.handle_command(Command::SaveAllBuffers {
            consent: Default::default(),
        });
        assert!(
            matches!(result.response, Response::ConflictsNeedConfirmation(_)),
            "{:?}",
            result.response
        );
        assert!(
            saved_files(&result.events).is_empty(),
            "{:?}",
            result.events
        );
    }

    // ── The save checks the file first (ADR-065: save-time mtime check) ─────
    // The watcher debounces for 150 ms, so a change can be on disk without the
    // conductor having been told. These tests never send `FileChanged`.

    #[test]
    fn save_over_an_unreported_disk_change_needs_confirmation() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        std::fs::write(tmp.path().join(FILE), SHELL_A).unwrap();

        let refused = conductor.handle_command(Command::SaveBuffer {
            path: editorial_types::SitePath::parse(FILE).expect("valid"),
            consent: Default::default(),
        });

        assert!(
            matches!(&refused.response, Response::ConflictsNeedConfirmation(files) if files == &vec![FILE.to_string()]),
            "{:?}",
            refused.response
        );
        assert!(
            refused
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::DiskConflict { file, .. } if file == FILE)),
            "the conflict is announced: {:?}",
            refused.events
        );
        assert_eq!(disk(&tmp, FILE), SHELL_A, "the disk text is intact");
        assert_eq!(
            conflicts(&conductor),
            vec![FILE.to_string()],
            "the conflict is recorded"
        );
        assert_eq!(title(&conductor), "Edited A", "the edit is kept");

        // With consent that names the file, the save goes through.
        let saved = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: consent(&[FILE]),
            })
            .response;
        assert!(matches!(saved, Response::Ok), "{saved:?}");
        assert!(disk(&tmp, FILE).contains("Edited A"));
        assert!(conflicts(&conductor).is_empty());
    }

    #[test]
    fn save_all_over_an_unreported_disk_change_needs_confirmation() {
        let tmp = build_site();
        std::fs::write(
            tmp.path().join(OTHER_FILE),
            "# Second Post\n\n----\n\nGamma.\n",
        )
        .unwrap();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        edit_title(&conductor, OTHER_FILE, "Edited B");
        std::fs::write(tmp.path().join(OTHER_FILE), SHELL_B).unwrap();

        let refused = conductor.handle_command(Command::SaveAllBuffers {
            consent: Default::default(),
        });

        assert!(
            matches!(&refused.response, Response::ConflictsNeedConfirmation(files) if files == &vec![OTHER_FILE.to_string()]),
            "{:?}",
            refused.response
        );
        assert!(
            refused.events.iter().any(
                |e| matches!(e, ConductorEvent::DiskConflict { file, .. } if file == OTHER_FILE)
            ),
            "{:?}",
            refused.events
        );
        assert_eq!(
            disk(&tmp, OTHER_FILE),
            SHELL_B,
            "the changed file is intact"
        );
        assert_eq!(
            disk(&tmp, FILE),
            CONTENT,
            "nothing is written, not even the clean file"
        );
        assert_eq!(conflicts(&conductor), vec![OTHER_FILE.to_string()]);
    }

    #[test]
    fn our_own_save_is_not_a_conflict_the_next_time() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        let first = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            })
            .response;
        assert!(matches!(first, Response::Ok), "{first:?}");

        edit_title(&conductor, FILE, "Edited again");
        let second = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            })
            .response;
        assert!(matches!(second, Response::Ok), "{second:?}");
        assert!(disk(&tmp, FILE).contains("Edited again"));
        assert!(conflicts(&conductor).is_empty());
    }

    #[test]
    fn saving_a_document_whose_file_was_deleted_needs_confirmation() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        std::fs::remove_file(tmp.path().join(FILE)).unwrap();

        let refused = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            })
            .response;
        assert!(
            matches!(refused, Response::ConflictsNeedConfirmation(_)),
            "{refused:?}"
        );
        assert!(!tmp.path().join(FILE).exists(), "nothing was written");

        let saved = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: consent(&[FILE]),
            })
            .response;
        assert!(matches!(saved, Response::Ok), "{saved:?}");
        assert!(
            disk(&tmp, FILE).contains("Edited A"),
            "the file is recreated on consent"
        );
    }

    #[test]
    fn reload_resolves_a_conflict() {
        let (conductor, tmp) = conflicted(false);
        assert_eq!(conflicts(&conductor), vec![FILE.to_string()]);

        let result = reload(&conductor, FILE);

        assert!(
            matches!(
                result.response,
                Response::Reloaded {
                    discarded: true,
                    ..
                }
            ),
            "{:?}",
            result.response
        );
        assert!(conflicts(&conductor).is_empty());
        assert!(!is_dirty(&conductor));
        assert_eq!(
            title(&conductor),
            "Shell Title",
            "the disk version is loaded"
        );
        assert_eq!(disk(&tmp, FILE), SHELL_A, "reload writes nothing");
    }

    // ── The base of unsaved edits (ADR-065 amendment F2) ───────────────────

    /// The base recorded for `file`'s unsaved edits, if any.
    fn base(conductor: &Conductor, file: &str) -> Option<String> {
        conductor
            .disk_bases
            .read()
            .unwrap()
            .get(Path::new(file))
            .map(str::to_string)
    }

    #[test]
    fn the_first_edit_records_the_file_as_the_base() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(base(&conductor, FILE), None, "a saved document has none");
        edit_title(&conductor, FILE, "Edited A");
        assert_eq!(base(&conductor, FILE).as_deref(), Some(CONTENT));
    }

    #[test]
    fn later_edits_keep_the_first_base() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_title(&conductor, FILE, "Edited A");
        // The file changes under N (not reported yet), then N is edited again:
        // the base is still the file the first edit was made on.
        std::fs::write(tmp.path().join(FILE), SHELL_A).unwrap();
        edit_title(&conductor, FILE, "Edited again");
        assert_eq!(base(&conductor, FILE).as_deref(), Some(CONTENT));
    }

    #[test]
    fn no_base_when_the_file_already_changed_under_n() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // The file is not the version N was read from any more.
        std::fs::write(tmp.path().join(FILE), SHELL_A).unwrap();
        edit_title(&conductor, FILE, "Edited A");
        assert!(is_dirty(&conductor));
        assert_eq!(base(&conductor, FILE), None);
    }

    #[test]
    fn saving_reloading_or_settling_clears_the_base() {
        let save = |conductor: &Conductor| {
            let response = conductor
                .handle_command(Command::SaveBuffer {
                    path: editorial_types::SitePath::parse(FILE).expect("valid"),
                    consent: Default::default(),
                })
                .response;
            assert!(matches!(response, Response::Ok), "{response:?}");
        };
        let save_all = |conductor: &Conductor| {
            let response = conductor
                .handle_command(Command::SaveAllBuffers {
                    consent: Default::default(),
                })
                .response;
            assert!(matches!(response, Response::Ok), "{response:?}");
        };
        let reload_it = |conductor: &Conductor| {
            reload(conductor, FILE);
        };
        let settle = |conductor: &Conductor| {
            // The file now reads as N: the edits are on disk.
            std::fs::write(
                conductor.site_dir.join(FILE),
                CONTENT.replace("First Post", "Edited A"),
            )
            .unwrap();
            conductor.handle_command(Command::FileChanged {
                paths: vec![editorial_types::SitePath::parse(FILE).expect("valid")],
            });
        };
        /// A way to end the unsaved state, by name.
        type Way<'a> = (&'a str, &'a dyn Fn(&Conductor));
        let ways: [Way; 4] = [
            ("save", &save),
            ("save all", &save_all),
            ("reload", &reload_it),
            ("settle", &settle),
        ];
        for (way, clear) in ways {
            let tmp = build_site();
            let conductor = make_conductor(&tmp);
            edit_title(&conductor, FILE, "Edited A");
            assert!(base(&conductor, FILE).is_some(), "{way}: recorded");
            clear(&conductor);
            assert!(!is_dirty(&conductor), "{way}: saved");
            assert_eq!(base(&conductor, FILE), None, "{way}");
            // The next edit starts from the file as it is now.
            edit_title(&conductor, FILE, "Edited B");
            assert_eq!(
                base(&conductor, FILE),
                Some(disk(&tmp, FILE)),
                "{way}: the next base"
            );
        }
    }

    /// ADR-043: every conductor write to a source file is recorded as a
    /// self-write, so the watcher's event for it is not an external change.
    #[test]
    fn create_content_write_is_recorded_as_self_write() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);

        let result = conductor.handle_command(Command::CreateContent {
            stem: "post".into(),
            slug: "new".into(),
        });
        assert!(
            !matches!(result.response, Response::Error(_)),
            "{:?}",
            result.response
        );

        let abs = tmp.path().join("content/post/new.md");
        let mtime = std::fs::metadata(&abs).unwrap().modified().unwrap();
        assert!(
            conductor.self_write_tracker.should_ignore(&abs, mtime),
            "the new file's write is a self-write"
        );
        // So the watcher's event for it changes nothing.
        let event = conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse("content/post/new.md").expect("valid")],
        });
        assert!(matches!(event.response, Response::Ok) && event.events.is_empty());
    }

    /// Creating content repopulates the store too: unsaved edits in the other
    /// documents survive it.
    #[test]
    fn create_content_keeps_unsaved_edits_in_other_documents() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);

        let program = format!(r#"(ned/set-text {TITLE_TEXT} "Edited A")"#);
        assert!(matches!(
            conductor.apply_ned_program(&program).response,
            Response::Applied { dirty_paths: 1, .. }
        ));

        let result = conductor.handle_command(Command::CreateContent {
            stem: "post".into(),
            slug: "new".into(),
        });
        assert!(
            !matches!(result.response, Response::Error(_)),
            "{:?}",
            result.response
        );
        assert!(
            tmp.path().join("content/post/new.md").exists(),
            "the file is created"
        );

        assert_eq!(title(&conductor), "Edited A", "A's unsaved edit survives");
        assert!(is_dirty(&conductor), "A is still dirty");
    }

    /// Guard: an accept is a one-step plan; it changes the document, marks it
    /// dirty and re-renders its page.
    #[test]
    fn accept_runs_through_execute_and_marks_dirty() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            TITLE_TEXT,
            NedMutation::SetText("Renamed Post".into()),
        );

        let result = accept(&conductor, &id);

        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert_eq!(title(&conductor), "Renamed Post");
        assert!(is_dirty(&conductor));
        assert!(accepted_pages(&result).contains(&"/post/first".to_string()));
    }

    /// A root of the caller's making and its own recorder, then the recorded
    /// plan through `execute_plan`. Marks the changed document dirty,
    /// re-renders its page, and returns the events to publish.
    #[test]
    fn run_ned_program_marks_dirty_and_rebuilds() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let recorder = evaluator::ned_primitives::PlanRecorder::new();
        let root = conductor.make_ned_root(&recorder).expect("root");
        let program = format!(r#"(ned/set-text {TITLE_TEXT} "Renamed Post")"#);

        let (value, outcome, events) = conductor
            .run_ned_program_with_events(&root, &recorder, &program)
            .expect("runs");

        assert!(matches!(value, template::Value::Absent), "{value:?}");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ConductorEvent::PagesRebuilt { .. })),
            "{events:?}"
        );
        assert_eq!(outcome.dirty_paths, vec![PathBuf::from(FILE)]);
        assert_eq!((outcome.applied, outcome.skipped), (1, 0));
        assert!(
            outcome.rebuilt_pages.contains(&"/post/first".to_string()),
            "{outcome:?}"
        );
        assert!(outcome.failed_pages.is_empty(), "{outcome:?}");
        assert!(is_dirty(&conductor));
        assert_eq!(title(&conductor), "Renamed Post");
    }

    #[test]
    fn accept_delete_marks_dirty() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(&conductor, FIRST_PARAGRAPH, NedMutation::Delete);

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert_eq!(body_texts(&conductor), vec!["Beta para.", "fn a() {}"]);
        assert!(is_dirty(&conductor));
    }

    #[test]
    fn accept_insert_after_marks_dirty_and_keeps_order() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let para = |t: &str| ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text(t));
        let id = create(
            &conductor,
            FIRST_PARAGRAPH,
            NedMutation::InsertAfter(vec![para("X."), para("Y.")]),
        );

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert_eq!(
            body_texts(&conductor),
            vec!["Alpha para.", "X.", "Y.", "Beta para.", "fn a() {}"]
        );
        assert!(is_dirty(&conductor));
    }

    #[test]
    fn accept_goes_stale_when_search_no_longer_unique() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            ALPHA_TEXT,
            NedMutation::SearchReplace {
                search: "Alpha".into(),
                replace: "Gamma".into(),
            },
        );

        // Edit the text so the search string occurs twice.
        {
            let root = live_root(&conductor);
            let mut store = conductor.node_store.write().unwrap();
            let body = node_store_bridge::content_bridge::find_child_by_name(&store, root, "body")
                .unwrap();
            let first = store.children(body)[0];
            let text = store.children(first)[0];
            store.replace_node(text, node_store::Node::Text("Alpha and Alpha.".into()));
        }

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert!(
            result
                .events
                .iter()
                .any(|ev| matches!(ev, ConductorEvent::NedSuggestionStaled { .. }))
        );
        match status(&conductor, &id) {
            NedSuggestionStatus::Stale { reason, cause } => {
                assert_eq!(cause, StaleCause::SearchNotUnique { occurrences: 2 });
                assert_eq!(reason, cause.to_string());
            }
            other => panic!("expected Stale, got {other:?}"),
        }
        assert_eq!(
            body_texts(&conductor)[0],
            "Alpha and Alpha.",
            "nothing applied"
        );
        assert!(!is_dirty(&conductor));
    }

    /// Guard: a suggestion whose postcondition fails at accept writes nothing.
    /// `prepare` takes `&NodeStore`, so the type enforces it; this pins it at
    /// the surface.
    #[test]
    fn failed_accept_leaves_store_unchanged() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            ALPHA_TEXT,
            NedMutation::SearchReplace {
                search: "Alpha".into(),
                replace: String::new(),
            },
        );

        // The paragraph is now only "Alpha": the replacement would empty it,
        // which is refused (ADR-059 L22: delete it instead).
        {
            let root = live_root(&conductor);
            let mut store = conductor.node_store.write().unwrap();
            let body = node_store_bridge::content_bridge::find_child_by_name(&store, root, "body")
                .unwrap();
            let first = store.children(body)[0];
            let text = store.children(first)[0];
            store.replace_node(text, node_store::Node::Text("Alpha".into()));
        }
        let (before, nodes_before) = {
            let root = live_root(&conductor);
            let store = conductor.node_store.read().unwrap();
            (
                node_store_bridge::serialize_from_store(&store, root).expect("serialize"),
                store.node_count(),
            )
        };

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        let stale = status(&conductor, &id);
        assert!(
            matches!(
                &stale,
                NedSuggestionStatus::Stale { cause: StaleCause::InvalidValue { detail }, .. }
                    if detail.contains("delete it instead")
            ),
            "{stale:?}"
        );
        let root = live_root(&conductor);
        let store = conductor.node_store.read().unwrap();
        assert_eq!(
            node_store_bridge::serialize_from_store(&store, root).expect("serialize"),
            before
        );
        assert_eq!(store.node_count(), nodes_before);
        assert!(!is_dirty(&conductor));
    }

    #[test]
    fn accept_after_target_deleted_is_stale_unresolved_and_persists_cause() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            ALPHA_TEXT,
            NedMutation::SetText("Changed.".into()),
        );

        delete_first_body_block(&conductor);

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert!(matches!(
            status(&conductor, &id),
            NedSuggestionStatus::Stale {
                cause: StaleCause::Unresolved,
                ..
            }
        ));

        let path = tmp
            .path()
            .join(".presemble/suggestions/ned")
            .join(format!("{id}.json"));
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            json["status"]["Stale"]["cause"], "Unresolved",
            "persisted: {json}"
        );
        assert!(
            json["status"]["Stale"]["reason"]
                .as_str()
                .is_some_and(|r| !r.is_empty())
        );
    }

    #[test]
    fn accept_goes_stale_when_selection_resolves_into_another_file() {
        const OTHER: &str = "content/post/second.md";
        let tmp = build_site();
        std::fs::write(
            tmp.path().join(OTHER),
            "# Second Post\n\n----\n\nGamma para.\n",
        )
        .unwrap();
        let conductor = make_conductor(&tmp);

        // Scoped over both bodies; at creation only A's text contains "Alpha".
        let selection = format!(
            r#"(ned/text-containing (ned/union (ned/body (ned/doc-by-path "{FILE}")) (ned/body (ned/doc-by-path "{OTHER}"))) "Alpha")"#
        );
        let id = create(
            &conductor,
            &selection,
            NedMutation::SetText("Changed.".into()),
        );

        // Edit the store so the selection now resolves uniquely into B.
        let other_text = {
            let root_a = live_root(&conductor);
            let root_b = conductor
                .doc_root_for_path(Path::new(OTHER))
                .expect("B root");
            let mut store = conductor.node_store.write().unwrap();
            let body_a =
                node_store_bridge::content_bridge::find_child_by_name(&store, root_a, "body")
                    .unwrap();
            let text_a = store.children(store.children(body_a)[0])[0];
            store.replace_node(text_a, node_store::Node::Text("Omega para.".into()));
            let body_b =
                node_store_bridge::content_bridge::find_child_by_name(&store, root_b, "body")
                    .unwrap();
            let text_b = store.children(store.children(body_b)[0])[0];
            store.replace_node(text_b, node_store::Node::Text("Alpha in B.".into()));
            text_b
        };

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert!(
            matches!(
                status(&conductor, &id),
                NedSuggestionStatus::Stale {
                    cause: StaleCause::Unresolved,
                    ..
                }
            ),
            "got {:?}",
            status(&conductor, &id)
        );
        let store = conductor.node_store.read().unwrap();
        assert!(
            matches!(store.get(other_text), Some(node_store::Node::Text(s)) if s == "Alpha in B."),
            "B must be unchanged: {:?}",
            store.get(other_text)
        );
        drop(store);
        assert!(
            !conductor
                .dirty_docs()
                .read()
                .unwrap()
                .contains(Path::new(OTHER)),
            "B must not be marked dirty"
        );
    }

    #[test]
    fn stale_suggestion_can_be_rejected_but_not_accepted() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            ALPHA_TEXT,
            NedMutation::SetText("Changed.".into()),
        );
        delete_first_body_block(&conductor);
        accept(&conductor, &id);
        assert!(matches!(
            status(&conductor, &id),
            NedSuggestionStatus::Stale { .. }
        ));

        let again = accept(&conductor, &id);
        match again.response {
            Response::Error(e) => assert!(e.contains("not pending"), "{e}"),
            other => panic!("accepting a Stale suggestion must fail, got {other:?}"),
        }

        let rejected = conductor.handle_command(Command::RejectNedSuggestion { id: id.clone() });
        assert!(
            matches!(rejected.response, Response::Ok),
            "got {:?}",
            rejected.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Rejected);
    }

    #[test]
    fn rebuild_page_then_accept_changes_the_live_root() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = create(
            &conductor,
            TITLE_TEXT,
            NedMutation::SetText("Live Title".into()),
        );

        let old_root = live_root(&conductor);
        conductor
            .rebuild_page(&tmp.path().join(FILE), CONTENT)
            .expect("rebuild_page");
        let new_root = live_root(&conductor);
        assert_ne!(old_root, new_root, "re-ingest creates a new live root");

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert_eq!(title(&conductor), "Live Title", "live root must be changed");

        // The superseded root is left untouched.
        let store = conductor.node_store.read().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, old_root, "preamble")
                .unwrap();
        let slot = store.children(preamble)[0];
        assert_eq!(block_text(&store, store.children(slot)[0]), "First Post");
    }

    /// `Stray para.` sits above `----` where the schema takes no paragraph:
    /// it lands in `preamble > unassigned` (ADR-060 P1).
    const CONTENT_WITH_UNASSIGNED: &str = "# First Post\n\nStray para.\n\n----\n\nAlpha para.\n";
    const STRAY_BLOCK: &str =
        r#"(ned/parents (ned/text-containing (ned/doc-by-path "content/post/first.md") "Stray"))"#;

    fn accept_and_save_unassigned(mutation: NedMutation) -> String {
        let tmp = build_site();
        std::fs::write(tmp.path().join(FILE), CONTENT_WITH_UNASSIGNED).unwrap();
        let conductor = make_conductor(&tmp);
        {
            let root = live_root(&conductor);
            let store = conductor.node_store.read().unwrap();
            let preamble =
                node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                    .expect("preamble");
            assert!(
                node_store_bridge::content_bridge::find_child_by_name(
                    &store,
                    preamble,
                    "unassigned"
                )
                .is_some(),
                "fixture must have an unassigned block"
            );
        }
        let id = create(&conductor, STRAY_BLOCK, mutation);
        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert!(is_dirty(&conductor));
        let saved = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            })
            .response;
        assert!(!matches!(saved, Response::Error(_)), "save: {saved:?}");
        std::fs::read_to_string(tmp.path().join(FILE)).expect("file written")
    }

    #[test]
    fn accept_delete_of_unassigned_block_removes_it_from_the_file() {
        let written = accept_and_save_unassigned(NedMutation::Delete);
        assert!(!written.contains("Stray para."), "written: {written}");
        assert!(written.contains("# First Post"), "written: {written}");
        assert!(written.contains("Alpha para."), "written: {written}");
    }

    #[test]
    fn accept_replace_of_unassigned_block_removes_it_from_the_file() {
        let heading = ned::NodeTree::element("heading")
            .with_attr("level", "3")
            .with_child(ned::NodeTree::text("Replaced"));
        let written = accept_and_save_unassigned(NedMutation::Replace(vec![heading]));
        assert!(!written.contains("Stray para."), "written: {written}");
        assert!(written.contains("### Replaced"), "written: {written}");
    }

    #[test]
    fn accept_multi_line_code_block_is_a_fixed_point_after_rebuild() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let code = "let x = 1;\nlet y = 2;\n";
        let id = create(&conductor, CODE_TEXT, NedMutation::SetText(code.into()));

        let result = accept(&conductor, &id);
        assert!(
            matches!(result.response, Response::Ok),
            "got {:?}",
            result.response
        );
        assert_eq!(status(&conductor, &id), NedSuggestionStatus::Accepted);
        assert_eq!(body_texts(&conductor)[2], code);

        let serialize = |c: &Conductor| {
            let root = live_root(c);
            let store = c.node_store.read().unwrap();
            node_store_bridge::serialize_from_store(&store, root).expect("serialize")
        };
        let first = serialize(&conductor);
        assert!(first.contains("let y = 2;"), "markdown: {first}");
        conductor
            .rebuild_page(&tmp.path().join(FILE), &first)
            .expect("rebuild_page from serialized markdown");
        let second = serialize(&conductor);
        assert_eq!(first, second, "file content must be a fixed point");
        assert_eq!(
            body_texts(&conductor)[2],
            code,
            "code survives the round trip"
        );
    }

    #[test]
    fn old_format_stale_suggestion_loads_with_unspecified_cause() {
        let tmp = build_site();
        let dir = tmp.path().join(".presemble/suggestions/ned");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sug-old.json"),
            r#"{"id":"sug-old","author":"Claude","file":"content/post/first.md","selection":"(x)","mutation":{"SetText":"y"},"workspace_hash":"abc","reason":"r","status":{"Stale":{"reason":"old reason"}},"created_at":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let conductor = make_conductor(&tmp);
        let id = SuggestionId::from("sug-old".to_string());
        assert_eq!(
            status(&conductor, &id),
            NedSuggestionStatus::Stale {
                reason: "old reason".to_string(),
                cause: StaleCause::Unspecified,
            }
        );
    }
}

#[cfg(test)]
mod block_edit_tests {
    use super::*;
    use editorial_types::{
        BlockEditCause, BlockEditIntent, BlockEditOutcome, BlockEditRequest, BlockEditResponse,
        BlockText, NedMutation, NedSuggestionStatus, StaleCause, SuggestionId,
    };
    use node_store_bridge::block_edit::Lowering;

    const FILE: &str = "content/post/first.md";
    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert></body></html>"#;
    const CONTENT: &str =
        "# First Post\n\n----\n\nAlpha para.\n\nBeta para.\n\n```rust\nfn a() {}\n```\n";

    const FIRST_PARAGRAPH: &str = r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/post/first.md"))) "paragraph" 0)"#;
    const TITLE_HEADING: &str = r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "title")) "heading" 0)"#;

    fn build_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("schemas/post")).unwrap();
        std::fs::create_dir_all(root.join("templates/post")).unwrap();
        std::fs::create_dir_all(root.join("content/post")).unwrap();
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), CONTENT).unwrap();
        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    fn request(
        file: &str,
        selection: &str,
        base: &str,
        proposed: &str,
        intent: BlockEditIntent,
    ) -> BlockEditRequest {
        BlockEditRequest {
            file: editorial_types::ContentPath::parse(file).expect("valid"),
            selection: selection.to_string(),
            base: BlockText::Markdown(base.to_string()),
            proposed: BlockText::Markdown(proposed.to_string()),
            intent,
            reason: Some("because".to_string()),
        }
    }

    fn edit(
        conductor: &Conductor,
        selection: &str,
        base: &str,
        proposed: &str,
        intent: BlockEditIntent,
    ) -> CommandResult {
        conductor.handle_command(Command::BlockEdit(request(
            FILE, selection, base, proposed, intent,
        )))
    }

    fn response(result: &CommandResult) -> BlockEditResponse {
        match &result.response {
            Response::BlockEdited(r) => r.clone(),
            other => panic!("expected BlockEdited, got {other:?}"),
        }
    }

    fn refused_cause(result: &CommandResult) -> BlockEditCause {
        match response(result) {
            BlockEditResponse::Refused(e) => e.cause,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn created_id(result: &CommandResult) -> SuggestionId {
        match response(result) {
            BlockEditResponse::Done(BlockEditOutcome::SuggestionCreated { id }) => id,
            other => panic!("expected SuggestionCreated, got {other:?}"),
        }
    }

    fn suggestion(conductor: &Conductor, id: &SuggestionId) -> editorial_types::NedSuggestion {
        conductor
            .ned_suggestions
            .read()
            .unwrap()
            .get(id)
            .expect("suggestion")
            .clone()
    }

    fn live_root(conductor: &Conductor) -> node_store::NodeId {
        conductor
            .doc_root_for_path(Path::new(FILE))
            .expect("live root")
    }

    fn block_text(store: &node_store::NodeStore, block: node_store::NodeId) -> String {
        match store.children(block).first().and_then(|t| store.get(*t)) {
            Some(node_store::Node::Text(s)) => s.clone(),
            other => panic!("expected a text child, got {other:?}"),
        }
    }

    fn body_blocks(conductor: &Conductor) -> Vec<node_store::NodeId> {
        let root = live_root(conductor);
        let store = conductor.node_store.read().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&store, root, "body")
            .expect("body");
        store.children(body)
    }

    fn body_texts(conductor: &Conductor) -> Vec<String> {
        let blocks = body_blocks(conductor);
        let store = conductor.node_store.read().unwrap();
        blocks.into_iter().map(|b| block_text(&store, b)).collect()
    }

    fn title(conductor: &Conductor) -> String {
        let root = live_root(conductor);
        let store = conductor.node_store.read().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                .expect("preamble");
        let slot = store.children(preamble)[0];
        block_text(&store, store.children(slot)[0])
    }

    /// Change the text of the first body block behind the conductor's back.
    fn manual_edit_first_paragraph(conductor: &Conductor, text: &str) {
        let block = body_blocks(conductor)[0];
        let mut store = conductor.node_store.write().unwrap();
        let text_node = store.children(block)[0];
        store.replace_node(text_node, node_store::Node::Text(text.to_string()));
    }

    fn pending_count(conductor: &Conductor) -> usize {
        conductor.ned_suggestions.read().unwrap().len()
    }

    // ── 11 ──────────────────────────────────────────────────────────────────

    #[test]
    fn apply_and_suggest_share_one_lowering() {
        let cases = [
            (FIRST_PARAGRAPH, "Alpha para.", "Alpha paragraph."),
            (FIRST_PARAGRAPH, "Alpha para.", "Alpha one.\n\nAlpha two."),
            (TITLE_HEADING, "First Post", "Second Post"),
        ];
        for (selection, base, proposed) in cases {
            // The one lowering both intents use.
            let tmp = build_site();
            let lowering_conductor = make_conductor(&tmp);
            let lowered = match lowering_conductor
                .lower_block_edit(&request(
                    FILE,
                    selection,
                    base,
                    proposed,
                    BlockEditIntent::Apply,
                ))
                .expect("lowers")
                .0
            {
                Lowering::Lowered(l) => l,
                Lowering::NoChange => panic!("{proposed:?} should change {base:?}"),
            };

            // Suggest stores exactly that mutation.
            let tmp_s = build_site();
            let suggester = make_conductor(&tmp_s);
            let id = created_id(&edit(
                &suggester,
                selection,
                base,
                proposed,
                BlockEditIntent::Suggest,
            ));
            let stored = suggestion(&suggester, &id);
            assert_eq!(stored.selection, lowered.selection, "{proposed:?}");
            assert_eq!(stored.mutation, lowered.mutation, "{proposed:?}");
            assert_eq!(
                stored.preview,
                Some(lowered.preview.clone()),
                "{proposed:?}"
            );

            // Apply has the same effect as accepting that suggestion.
            let tmp_a = build_site();
            let applier = make_conductor(&tmp_a);
            let applied = edit(&applier, selection, base, proposed, BlockEditIntent::Apply);
            assert!(
                matches!(
                    response(&applied),
                    BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
                ),
                "{:?}",
                response(&applied)
            );
            let accepted = suggester.handle_command(Command::AcceptNedSuggestion { id });
            assert!(
                matches!(accepted.response, Response::Ok),
                "{:?}",
                accepted.response
            );
            assert_eq!(title(&applier), title(&suggester), "{proposed:?}");
            assert_eq!(body_texts(&applier), body_texts(&suggester), "{proposed:?}");
        }
    }

    /// A site whose title is `# Hello *World*`.
    fn formatted_title_site() -> tempfile::TempDir {
        let tmp = build_site();
        std::fs::write(
            tmp.path().join(FILE),
            "# Hello *World*\n\n----\n\nAlpha para.\n\nBeta para.\n",
        )
        .unwrap();
        tmp
    }

    fn saved_file(tmp: &tempfile::TempDir, conductor: &Conductor) -> String {
        let result = conductor.handle_command(Command::SaveBuffer {
            path: editorial_types::SitePath::parse(FILE).expect("valid"),
            consent: Default::default(),
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        std::fs::read_to_string(tmp.path().join(FILE)).expect("content file")
    }

    /// BlockEdit Apply, and Suggest then accept, on `Hello *World*` write the
    /// same bytes to disk, for a text change, a formatting change and both.
    #[test]
    fn heading_block_edit_apply_equals_suggest_then_accept_on_disk() {
        let cases = [
            (
                "Hello *Presemble*",
                "# Hello *Presemble*\n\n----\n\nAlpha para.\n\nBeta para.\n",
            ),
            (
                "Hello **World**",
                "# Hello **World**\n\n----\n\nAlpha para.\n\nBeta para.\n",
            ),
            (
                "Howdy **World**",
                "# Howdy **World**\n\n----\n\nAlpha para.\n\nBeta para.\n",
            ),
        ];
        for (proposed, expected) in cases {
            let tmp_a = formatted_title_site();
            let applier = make_conductor(&tmp_a);
            let applied = edit(
                &applier,
                TITLE_HEADING,
                "Hello *World*",
                proposed,
                BlockEditIntent::Apply,
            );
            assert!(
                matches!(
                    response(&applied),
                    BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
                ),
                "{proposed:?}: {:?}",
                response(&applied)
            );

            let tmp_s = formatted_title_site();
            let suggester = make_conductor(&tmp_s);
            let id = created_id(&edit(
                &suggester,
                TITLE_HEADING,
                "Hello *World*",
                proposed,
                BlockEditIntent::Suggest,
            ));
            let accepted = suggester.handle_command(Command::AcceptNedSuggestion { id });
            assert!(
                matches!(accepted.response, Response::Ok),
                "{proposed:?}: {:?}",
                accepted.response
            );

            let (a, s) = (saved_file(&tmp_a, &applier), saved_file(&tmp_s, &suggester));
            assert_eq!(a, expected, "apply {proposed:?}");
            assert_eq!(s, a, "suggest then accept {proposed:?}");
        }
    }

    /// A site whose first body paragraph is `See ![c](c.png) *here*.`.
    fn formatted_paragraph_site() -> tempfile::TempDir {
        let tmp = build_site();
        std::fs::write(
            tmp.path().join(FILE),
            "# First Post\n\n----\n\nSee ![c](c.png) *here*.\n\nBeta para.\n",
        )
        .unwrap();
        tmp
    }

    /// BlockEdit Apply, and Suggest then accept, on a paragraph with an inline
    /// image and emphasis write the same bytes to disk, for a text change, a
    /// formatting change and both. The image and the formatting survive: the
    /// text is never rewritten as markdown source.
    #[test]
    fn paragraph_block_edit_apply_equals_suggest_then_accept_on_disk() {
        const BASE: &str = "See ![c](c.png) *here*.";
        let cases = [
            (
                "See ![c](c.png) *there*.",
                "# First Post\n\n----\n\nSee ![c](c.png) *there*.\n\nBeta para.\n",
            ),
            (
                "See ![c](c.png) **here**.",
                "# First Post\n\n----\n\nSee ![c](c.png) **here**.\n\nBeta para.\n",
            ),
            (
                "Look ![c](c.png) **there**!",
                "# First Post\n\n----\n\nLook ![c](c.png) **there**!\n\nBeta para.\n",
            ),
        ];
        for (proposed, expected) in cases {
            let tmp_a = formatted_paragraph_site();
            let applier = make_conductor(&tmp_a);
            let applied = edit(
                &applier,
                FIRST_PARAGRAPH,
                BASE,
                proposed,
                BlockEditIntent::Apply,
            );
            assert!(
                matches!(
                    response(&applied),
                    BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
                ),
                "{proposed:?}: {:?}",
                response(&applied)
            );

            let tmp_s = formatted_paragraph_site();
            let suggester = make_conductor(&tmp_s);
            let id = created_id(&edit(
                &suggester,
                FIRST_PARAGRAPH,
                BASE,
                proposed,
                BlockEditIntent::Suggest,
            ));
            let accepted = suggester.handle_command(Command::AcceptNedSuggestion { id });
            assert!(
                matches!(accepted.response, Response::Ok),
                "{proposed:?}: {:?}",
                accepted.response
            );

            let (a, s) = (saved_file(&tmp_a, &applier), saved_file(&tmp_s, &suggester));
            assert_eq!(a, expected, "apply {proposed:?}");
            assert_eq!(s, a, "suggest then accept {proposed:?}");
        }
    }

    /// A level change replaces the title block. The page's references follow
    /// it (ADR-059 §6: references are recomputed when an affected document is
    /// rebuilt): the semantic title and link resolve, and the page renders the
    /// new title.
    #[test]
    fn replacing_a_slot_title_keeps_page_references() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let mut req = request(FILE, TITLE_HEADING, "", "", BlockEditIntent::Apply);
        req.base = BlockText::Edn("[:heading {:level \"1\"} \"First Post\"]".to_string());
        req.proposed = BlockText::Edn("[:heading {:level \"2\"} \"Second Post\"]".to_string());

        let result = conductor.handle_command(Command::BlockEdit(req));
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );

        let sem = conductor
            .semantic_content_by_url("/post/first")
            .expect("the page has semantic content");
        {
            let store = conductor.node_store.read().unwrap();
            let part = |name: &str| {
                store
                    .consists_of(sem)
                    .into_iter()
                    .chain(store.references(sem))
                    .find(|(n, _)| store.resolve_name(*n) == name)
                    .map(|(_, id)| id)
            };
            let title = part("title").expect("the title is referenced");
            assert_eq!(
                content_store::heading_plain_text(&store, title).as_deref(),
                Some("Second Post"),
                "the reference is the live heading, not the removed one"
            );
            let link = part("link").expect("the title link exists");
            assert_eq!(
                node_store_bridge::content_bridge::find_attr_text(&store, link, "text").as_deref(),
                Some("Second Post")
            );
        }
        let html = std::fs::read_to_string(site_index::output_path_for_stem_slug(
            &conductor.output_dir,
            "post",
            "first",
        ))
        .expect("the page is rendered");
        assert!(html.contains("Second Post"), "{html}");
        assert!(!html.contains("First Post"), "{html}");
    }

    /// `build_site` plus a root page that lists the post by link.
    fn site_with_listing() -> tempfile::TempDir {
        let tmp = build_site();
        let root = tmp.path();
        std::fs::write(
            root.join("schemas/index.md"),
            "[<name>](/post/<name>) {#highlight}\ntype\n: link(post)\noccurs\n: 1..6\n\n----\n\nBody.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("templates/index.html"),
            r#"<ul><template data-each="(ref input.highlight)"><li><presemble:insert data="(ref item.title)" as="h3" /></li></template></ul>"#,
        )
        .unwrap();
        std::fs::write(
            root.join("content/index.md"),
            "[First Post](/post/first)\n\n----\n\nIndex body.\n",
        )
        .unwrap();
        tmp
    }

    fn change_title_level(conductor: &Conductor) {
        let mut req = request(FILE, TITLE_HEADING, "", "", BlockEditIntent::Apply);
        req.base = BlockText::Edn("[:heading {:level \"1\"} \"First Post\"]".to_string());
        req.proposed = BlockText::Edn("[:heading {:level \"2\"} \"Second Post\"]".to_string());
        let result = conductor.handle_command(Command::BlockEdit(req));
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
    }

    /// A page that lists a post shows the post's new title after a structural
    /// edit of that title: the listing follows the post's semantic content.
    #[test]
    fn replacing_a_slot_title_updates_the_collection_listing() {
        let tmp = site_with_listing();
        let conductor = make_conductor(&tmp);
        change_title_level(&conductor);

        let listing = std::fs::read_to_string(site_index::output_path_for_stem_slug(
            &conductor.output_dir,
            "",
            "index",
        ))
        .expect("the listing page is rendered");
        assert!(listing.contains("Second Post"), "{listing}");
        assert!(!listing.contains("First Post"), "{listing}");
    }

    /// EDN is the whole block, so a different level on a slot title is a
    /// Replace and the file shows it, even though the slot's level rule is
    /// broken: invalid content is saved and reported, never refused. (In
    /// markdown the level belongs to the schema, and the same change is
    /// NoChange.)
    #[test]
    fn edn_level_change_on_a_slot_heading_is_replace() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let mut req = request(FILE, TITLE_HEADING, "", "", BlockEditIntent::Apply);
        req.base = BlockText::Edn("[:heading {:level \"1\"} \"First Post\"]".to_string());
        req.proposed = BlockText::Edn("[:heading {:level \"2\"} \"First Post\"]".to_string());

        let result = conductor.handle_command(Command::BlockEdit(req));
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
        let file = saved_file(&tmp, &conductor);
        assert!(file.starts_with("## First Post\n"), "{file}");

        // Suggest, then accept, writes the same file.
        let tmp_s = build_site();
        let suggester = make_conductor(&tmp_s);
        let mut req = request(FILE, TITLE_HEADING, "", "", BlockEditIntent::Suggest);
        req.base = BlockText::Edn("[:heading {:level \"1\"} \"First Post\"]".to_string());
        req.proposed = BlockText::Edn("[:heading {:level \"2\"} \"First Post\"]".to_string());
        let id = created_id(&suggester.handle_command(Command::BlockEdit(req)));
        let accepted = suggester.handle_command(Command::AcceptNedSuggestion { id });
        assert!(
            matches!(accepted.response, Response::Ok),
            "{:?}",
            accepted.response
        );
        assert_eq!(saved_file(&tmp_s, &suggester), file);
    }

    /// When the references of a page cannot be rebuilt after a structural
    /// edit, the page has a build error and is reported in `BuildFailed`; it
    /// is not left silently stale.
    #[test]
    fn failed_reference_refresh_is_a_build_error() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        conductor
            .url_to_semantic
            .write()
            .unwrap()
            .remove("/post/first");

        let mut req = request(FILE, TITLE_HEADING, "", "", BlockEditIntent::Apply);
        req.base = BlockText::Edn("[:heading {:level \"1\"} \"First Post\"]".to_string());
        req.proposed = BlockText::Edn("[:heading {:level \"2\"} \"Second Post\"]".to_string());
        let result = conductor.handle_command(Command::BlockEdit(req));

        assert!(
            result.events.iter().any(|e| matches!(
                e,
                ConductorEvent::BuildFailed { error_pages } if error_pages.iter().any(|p| p == "/post/first")
            )),
            "{:?}",
            result.events
        );
        let errors = conductor.build_errors.read().unwrap();
        let messages = errors
            .get("/post/first")
            .expect("a build error for the page");
        assert!(
            messages
                .iter()
                .any(|m| m.contains("references could not be rebuilt")),
            "{messages:?}"
        );
    }

    /// A structural edit changes the blocks it names and nothing else: the
    /// untouched blocks and the page's semantic node keep their ids.
    #[test]
    fn structural_edit_keeps_other_blocks_node_ids() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let blocks_before = body_blocks(&conductor);
        let sem_before = conductor
            .semantic_content_by_url("/post/first")
            .expect("semantic node");
        let untouched = blocks_before[1];

        let applied = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha one.\n\nAlpha two.",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&applied),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&applied)
        );

        assert!(
            body_blocks(&conductor).contains(&untouched),
            "the sibling block kept its node id"
        );
        assert_eq!(
            conductor.semantic_content_by_url("/post/first"),
            Some(sem_before)
        );
    }

    /// A guard, green on arrival: it pins that an EDN edit and an EDN
    /// suggestion differ only by approval, and that a stored preview is
    /// markdown.
    #[test]
    fn edn_block_edit_suggest_then_accept_equals_apply() {
        let edn_request = |intent| {
            let mut req = request(FILE, FIRST_PARAGRAPH, "", "", intent);
            req.base = BlockText::Edn("[:paragraph \"Alpha para.\"]".to_string());
            req.proposed = BlockText::Edn("[:paragraph \"Alpha paragraph.\"]".to_string());
            req
        };
        let saved = |tmp: &tempfile::TempDir, conductor: &Conductor| {
            let result = conductor.handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            });
            assert!(
                matches!(result.response, Response::Ok),
                "{:?}",
                result.response
            );
            std::fs::read_to_string(tmp.path().join(FILE)).expect("content file")
        };

        let tmp_a = build_site();
        let applier = make_conductor(&tmp_a);
        let applied =
            applier.handle_command(Command::BlockEdit(edn_request(BlockEditIntent::Apply)));
        assert!(
            matches!(
                response(&applied),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&applied)
        );

        let tmp_s = build_site();
        let suggester = make_conductor(&tmp_s);
        let id = created_id(
            &suggester.handle_command(Command::BlockEdit(edn_request(BlockEditIntent::Suggest))),
        );
        assert_eq!(
            suggestion(&suggester, &id).preview,
            Some(editorial_types::SuggestionPreview {
                base: editorial_types::PreviewText::new("Alpha para."),
                proposed: editorial_types::PreviewText::new("Alpha paragraph."),
            })
        );
        let accepted = suggester.handle_command(Command::AcceptNedSuggestion { id });
        assert!(
            matches!(accepted.response, Response::Ok),
            "{:?}",
            accepted.response
        );

        let on_disk_applied = saved(&tmp_a, &applier);
        let on_disk_accepted = saved(&tmp_s, &suggester);
        assert!(
            on_disk_applied.contains("Alpha paragraph."),
            "{on_disk_applied}"
        );
        assert_eq!(on_disk_applied, on_disk_accepted);
    }

    // ── 12 ──────────────────────────────────────────────────────────────────

    #[test]
    fn block_edit_apply_text_change_rebuilds_page() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let result = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha paragraph.",
            BlockEditIntent::Apply,
        );
        let pages = match response(&result) {
            BlockEditResponse::Done(BlockEditOutcome::Applied { pages }) => pages,
            other => panic!("expected Applied, got {other:?}"),
        };
        assert!(
            pages.contains(&"/post/first".to_string()),
            "pages: {pages:?}"
        );
        assert_eq!(body_texts(&conductor)[0], "Alpha paragraph.");
        assert!(
            conductor
                .dirty_docs()
                .read()
                .unwrap()
                .contains(Path::new(FILE))
        );
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::PagesRebuilt { .. })),
            "events: {:?}",
            result.events
        );
        assert_eq!(pending_count(&conductor), 0, "apply creates no suggestion");
    }

    /// ADR-046: a body edit scrolls the browser to the edited block. The
    /// anchor names the block's slot, kind and offset among its kind.
    #[test]
    fn block_edit_apply_on_a_body_block_broadcasts_its_anchor() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let second_paragraph = FIRST_PARAGRAPH.replace("\"paragraph\" 0", "\"paragraph\" 1");
        let result = edit(
            &conductor,
            &second_paragraph,
            "Beta para.",
            "Beta paragraph.",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
        let anchors: Vec<_> = result
            .events
            .iter()
            .filter_map(|e| match e {
                ConductorEvent::PagesRebuilt { anchor, .. } => Some(anchor.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(anchors.len(), 1, "events: {:?}", result.events);
        let anchor = anchors[0]
            .as_ref()
            .expect("a body block edit carries an anchor");
        assert_eq!(anchor.slot, "body");
        assert_eq!(anchor.node_kind, "paragraph");
        assert_eq!(anchor.offset, 1, "the second paragraph");
    }

    /// A slot block has no body index, so there is no anchor for it.
    #[test]
    fn block_edit_apply_on_a_slot_block_has_no_anchor() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let result = edit(
            &conductor,
            TITLE_HEADING,
            "First Post",
            "Second Post",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::PagesRebuilt { anchor: None, .. })),
            "events: {:?}",
            result.events
        );
    }

    /// ADR-062 D5: replacing a block keeps the comment above it.
    #[test]
    fn block_edit_apply_keeps_the_comment_above_the_replaced_paragraph() {
        let tmp = build_site();
        std::fs::write(
            tmp.path().join(FILE),
            "# First Post\n\n----\n\n<!-- keep me -->\n\nAlpha para.\n\nBeta para.\n",
        )
        .unwrap();
        let conductor = make_conductor(&tmp);
        let result = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "New body paragraph.",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
        let doc_root = conductor
            .doc_root_for_path(Path::new(FILE))
            .expect("document in store");
        let store_arc = conductor.node_store();
        let store = store_arc.read().unwrap();
        let saved = node_store_bridge::serialize_from_store(&store, doc_root).expect("saves");
        assert_eq!(
            saved,
            "# First Post\n\n----\n\n<!-- keep me -->\n\nNew body paragraph.\n\nBeta para.\n"
        );
    }

    // ── 13 ──────────────────────────────────────────────────────────────────

    #[test]
    fn block_edit_apply_structural_change_replaces_block() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let result = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha one.\n\nAlpha two.",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
        assert_eq!(
            body_texts(&conductor),
            vec!["Alpha one.", "Alpha two.", "Beta para.", "fn a() {}"]
        );
    }

    // ── 14 ──────────────────────────────────────────────────────────────────

    #[test]
    fn block_edit_suggest_structural_change_creates_replace_suggestion() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let result = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha one.\n\nAlpha two.",
            BlockEditIntent::Suggest,
        );
        let id = created_id(&result);
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::NedSuggestionCreated { .. })),
            "events: {:?}",
            result.events
        );
        let sug = suggestion(&conductor, &id);
        assert!(matches!(&sug.mutation, NedMutation::Replace(trees) if trees.len() == 2));
        assert_eq!(sug.selection, FIRST_PARAGRAPH);
        assert_eq!(sug.status, NedSuggestionStatus::Pending);
        assert_eq!(sug.reason, "because");
        assert_eq!(
            sug.author,
            editorial_types::Author::Human("browser".to_string())
        );
        assert_eq!(
            sug.preview,
            Some(editorial_types::SuggestionPreview {
                base: editorial_types::PreviewText::new("Alpha para."),
                proposed: editorial_types::PreviewText::new("Alpha one.\n\nAlpha two."),
            })
        );
        assert_eq!(
            body_texts(&conductor)[0],
            "Alpha para.",
            "suggest must not change the block"
        );
    }

    // ── 15 ──────────────────────────────────────────────────────────────────

    #[test]
    fn block_edit_selection_must_resolve_to_one_block_in_file() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let doc = r#"(ned/doc-by-path "content/post/first.md")"#;
        let bad_selections = [
            // A text node, not a block.
            format!("(ned/text-in {FIRST_PARAGRAPH})"),
            // A container.
            format!("(ned/body {doc})"),
            // Nothing.
            format!(r#"(ned/nth-of-kind (ned/children (ned/body {doc})) "paragraph" 9)"#),
            // Not evaluable.
            "(ned/no-such-thing)".to_string(),
        ];
        for sel in &bad_selections {
            for intent in [BlockEditIntent::Apply, BlockEditIntent::Suggest] {
                let result = edit(&conductor, sel, "Alpha para.", "Changed.", intent);
                assert_eq!(
                    refused_cause(&result),
                    BlockEditCause::Unaddressable,
                    "{sel}"
                );
            }
        }
        // A block in another file than the request names.
        let result = conductor.handle_command(Command::BlockEdit(request(
            "content/post/other.md",
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Changed.",
            BlockEditIntent::Apply,
        )));
        assert_eq!(refused_cause(&result), BlockEditCause::Unaddressable);
        assert_eq!(body_texts(&conductor)[0], "Alpha para.");
        assert_eq!(pending_count(&conductor), 0);
    }

    #[test]
    fn block_edit_refusals_drift_no_change_and_not_editable() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let drifted = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Old text.",
            "New.",
            BlockEditIntent::Apply,
        );
        assert_eq!(refused_cause(&drifted), BlockEditCause::Drifted);
        let same = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha para.",
            BlockEditIntent::Apply,
        );
        assert_eq!(
            response(&same),
            BlockEditResponse::Done(BlockEditOutcome::NoChange)
        );
        let empty = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "  ",
            BlockEditIntent::Suggest,
        );
        assert_eq!(refused_cause(&empty), BlockEditCause::Invalid);
    }

    // ── already in place (2e-2a) ────────────────────────────────────────────

    fn dirty_buffers(conductor: &Conductor) -> Vec<String> {
        match conductor.handle_command(Command::GetDirtyBuffers).response {
            Response::DirtyBuffers(paths) => paths,
            other => panic!("expected DirtyBuffers, got: {other:?}"),
        }
    }

    #[test]
    fn accept_of_a_block_edit_suggestion_already_in_place_is_accepted_without_rebuild() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = created_id(&edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha paragraph.",
            BlockEditIntent::Suggest,
        ));
        // The same change lands another way, and is saved: nothing is dirty.
        let applied = edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha paragraph.",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&applied),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&applied)
        );
        let saved = conductor.handle_command(Command::SaveBuffer {
            path: editorial_types::SitePath::parse(FILE).expect("valid"),
            consent: Default::default(),
        });
        assert!(
            matches!(saved.response, Response::Ok),
            "{:?}",
            saved.response
        );
        assert!(dirty_buffers(&conductor).is_empty());

        let result = conductor.handle_command(Command::AcceptNedSuggestion { id: id.clone() });

        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(
            suggestion(&conductor, &id).status,
            NedSuggestionStatus::Accepted
        );
        assert!(
            result.events.iter().any(|e| matches!(
                e,
                ConductorEvent::NedSuggestionAccepted { pages, .. } if pages.is_empty()
            )),
            "events: {:?}",
            result.events
        );
        assert!(
            !result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::PagesRebuilt { .. })),
            "nothing is rebuilt: {:?}",
            result.events
        );
        assert!(dirty_buffers(&conductor).is_empty(), "nothing is dirty");
        assert_eq!(body_texts(&conductor)[0], "Alpha paragraph.");
    }

    #[test]
    fn block_edit_whose_proposal_is_already_in_place_is_no_change() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        for intent in [BlockEditIntent::Apply, BlockEditIntent::Suggest] {
            // The block already reads as the proposal; the client's base is
            // stale. Nothing to do, so nothing to refuse.
            let result = edit(
                &conductor,
                FIRST_PARAGRAPH,
                "Old text.",
                "Alpha para.",
                intent,
            );
            assert_eq!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::NoChange),
                "{intent:?}"
            );
            assert!(result.events.is_empty(), "{intent:?}: {:?}", result.events);
        }
        assert_eq!(pending_count(&conductor), 0);
        assert!(dirty_buffers(&conductor).is_empty());
        assert_eq!(body_texts(&conductor)[0], "Alpha para.");
    }

    // ── 16 ──────────────────────────────────────────────────────────────────

    #[test]
    fn structural_suggestion_accept_after_manual_edit_is_drifted() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let id = created_id(&edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha para.",
            "Alpha one.\n\nAlpha two.",
            BlockEditIntent::Suggest,
        ));
        manual_edit_first_paragraph(&conductor, "Alpha rewritten.");

        let result = conductor.handle_command(Command::AcceptNedSuggestion { id: id.clone() });
        assert!(
            result
                .events
                .iter()
                .any(|e| matches!(e, ConductorEvent::NedSuggestionStaled { .. })),
            "events: {:?}",
            result.events
        );
        assert!(
            matches!(
                suggestion(&conductor, &id).status,
                NedSuggestionStatus::Stale {
                    cause: StaleCause::Drifted,
                    ..
                }
            ),
            "{:?}",
            suggestion(&conductor, &id).status
        );
        assert_eq!(
            body_texts(&conductor)[0],
            "Alpha rewritten.",
            "stale accept changes nothing"
        );
    }

    // ── 17 ──────────────────────────────────────────────────────────────────

    #[test]
    fn search_replace_suggestion_survives_unrelated_edit() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        manual_edit_first_paragraph(&conductor, "Alpha beta gamma");
        let id = created_id(&edit(
            &conductor,
            FIRST_PARAGRAPH,
            "Alpha beta gamma",
            "Alpha BETA gamma",
            BlockEditIntent::Suggest,
        ));
        let sug = suggestion(&conductor, &id);
        assert!(
            matches!(sug.mutation, NedMutation::SearchReplace { .. }),
            "{:?}",
            sug.mutation
        );
        assert!(sug.preview.is_some());

        // The block changes elsewhere: the base has drifted, but the search
        // text is still unique, so SearchReplace has no base check.
        manual_edit_first_paragraph(&conductor, "Alpha beta delta");
        let result = conductor.handle_command(Command::AcceptNedSuggestion { id: id.clone() });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert_eq!(
            suggestion(&conductor, &id).status,
            NedSuggestionStatus::Accepted
        );
        assert_eq!(body_texts(&conductor)[0], "Alpha BETA delta");
    }

    // ── 18 ──────────────────────────────────────────────────────────────────

    #[test]
    fn forbidden_payload_kind_is_refused_for_both_intents() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        for proposed in [
            "<div class=\"note\">\nAlpha para.\n</div>",
            "Alpha para.\n\n---",
        ] {
            for intent in [BlockEditIntent::Apply, BlockEditIntent::Suggest] {
                let result = edit(&conductor, FIRST_PARAGRAPH, "Alpha para.", proposed, intent);
                assert_eq!(
                    refused_cause(&result),
                    BlockEditCause::Invalid,
                    "{proposed:?}"
                );
            }
        }
        assert_eq!(
            body_texts(&conductor),
            vec!["Alpha para.", "Beta para.", "fn a() {}"]
        );
        assert_eq!(pending_count(&conductor), 0);
    }

    // ── F1: the base is rechecked under the write lock ──────────────────────

    #[test]
    fn apply_rechecks_base_under_the_write_lock() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        for proposed in ["Alpha paragraph.", "Alpha one.\n\nAlpha two."] {
            let req = request(
                FILE,
                FIRST_PARAGRAPH,
                "Alpha para.",
                proposed,
                BlockEditIntent::Apply,
            );
            let lowered = match conductor.lower_block_edit(&req).expect("lowers").0 {
                Lowering::Lowered(l) => l,
                Lowering::NoChange => panic!("should change"),
            };
            // The block changes between the lowering and the apply.
            // (A Replace is caught by the base check; a SearchReplace by its
            // search no longer matching, which also means drift.)
            manual_edit_first_paragraph(&conductor, "Totally different");
            let result = conductor.run_lowered(
                BlockEditIntent::Apply,
                &editorial_types::ContentPath::parse(FILE).expect("valid"),
                lowered,
                None,
                None,
            );
            assert_eq!(
                refused_cause(&result),
                BlockEditCause::Drifted,
                "{proposed:?}"
            );
            assert_eq!(body_texts(&conductor)[0], "Totally different");
            manual_edit_first_paragraph(&conductor, "Alpha para.");
        }
    }

    // ── F3: search failures on Apply mean drift ─────────────────────────────

    #[test]
    fn apply_refusal_causes() {
        use editorial_types::StaleCause as S;
        let cause = Conductor::apply_refusal_cause;
        assert_eq!(cause(&S::Drifted), BlockEditCause::Drifted);
        assert_eq!(cause(&S::SearchNotFound), BlockEditCause::Drifted);
        assert_eq!(
            cause(&S::SearchNotUnique { occurrences: 2 }),
            BlockEditCause::Drifted
        );
        assert_eq!(cause(&S::Unresolved), BlockEditCause::Unaddressable);
        assert_eq!(
            cause(&S::Ambiguous { matches: 2 }),
            BlockEditCause::Unaddressable
        );
        assert_eq!(
            cause(&S::EvalError { detail: "x".into() }),
            BlockEditCause::Unaddressable
        );
        assert_eq!(
            cause(&S::InvalidValue { detail: "x".into() }),
            BlockEditCause::Invalid
        );
        assert_eq!(cause(&S::NoChange), BlockEditCause::Invalid);
    }

    // ── slot heading under both intents (Content projection) ────────────────

    #[test]
    fn block_edit_on_slot_heading_uses_content_projection() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // The base is the heading's source, with no `#` markers.
        let result = edit(
            &conductor,
            TITLE_HEADING,
            "First Post",
            "Second Post",
            BlockEditIntent::Apply,
        );
        assert!(
            matches!(
                response(&result),
                BlockEditResponse::Done(BlockEditOutcome::Applied { .. })
            ),
            "{:?}",
            response(&result)
        );
        assert_eq!(title(&conductor), "Second Post");
    }
}

#[cfg(test)]
mod apply_ned_program_diagnostic_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    /// Build a minimal conductor with a document loaded into the NodeStore.
    ///
    /// Document at `content/test/doc.md` has a "title" slot with a heading.
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let doc = Document {
            preamble: im::vector![DocumentSlot {
                name: SlotName::new("title"),
                elements: im::vector![Spanned {
                    node: ContentElement::heading_from_markdown(
                        HeadingLevel::new(1).unwrap(),
                        "Test Title",
                    ),
                    span: Span { start: 0, end: 0 },
                }],
            }],
            body: im::vector![],
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };

        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: "content/test/doc.md".to_string(),
            page_kind: "item".to_string(),
        };

        {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta));
        }

        (conductor, tmp)
    }

    /// A program that returns a number (not a Selection) should produce
    /// `Response::Applied` with `dirty_paths: 0`.
    #[test]
    fn apply_no_selection_returns_zero_dirty_paths() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Evaluate a program that returns a number, not a Selection.
        let result = conductor.apply_ned_program("42");

        match result.response {
            Response::Applied {
                dirty_paths,
                rebuilt_pages,
                failed_pages,
            } => {
                assert_eq!(
                    dirty_paths, 0,
                    "non-Selection program should report 0 dirty paths"
                );
                assert!(
                    rebuilt_pages.is_empty(),
                    "no pages should be rebuilt for a no-op"
                );
                assert!(failed_pages.is_empty(), "no pages should fail for a no-op");
            }
            other => panic!("expected Response::Applied, got: {other:?}"),
        }
    }

    /// A program that mutates a document node should produce `Response::Applied`
    /// with `dirty_paths: 1` (the document was marked dirty).
    #[test]
    fn apply_with_selection_returns_correct_dirty_paths_count() {
        let (conductor, _tmp) = make_conductor_with_doc();

        // Program: set the title slot text — this returns a Selection over the mutated nodes.
        let program = r#"(ned/set-text (-> (ned/slot (ned/doc-by-path "content/test/doc.md") "title") ned/descendants ned/texts) "Changed")"#;

        let result = conductor.apply_ned_program(program);

        match result.response {
            Response::Applied { dirty_paths, .. } => {
                assert_eq!(
                    dirty_paths, 1,
                    "mutation affecting one document should report dirty_paths: 1"
                );
            }
            Response::Error(e) => panic!("unexpected error from apply_ned_program: {e}"),
            other => panic!("expected Response::Applied, got: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// NED programs are plans (ADR-059 stage 2e): select against the store as it
// was before the program, then apply all or nothing.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod ned_program_plan_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    const FILE: &str = "content/test/doc.md";
    const OTHER: &str = "content/test/other.md";
    const TITLE_TEXT: &str = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/doc.md") "title")))"#;
    const OTHER_TITLE_TEXT: &str = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/test/other.md") "title")))"#;
    const BODY: &str = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "body")"#;
    const SUMMARY: &str = r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "summary")"#;

    fn spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned {
            node,
            span: Span { start: 0, end: 0 },
        }
    }

    fn p(text: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::paragraph_from_markdown(text))
    }

    fn title(text: &str) -> DocumentSlot {
        DocumentSlot {
            name: SlotName::new("title"),
            elements: im::vector![spanned(ContentElement::heading_from_markdown(
                HeadingLevel::new(1).unwrap(),
                text,
            ))],
        }
    }

    fn add_doc(conductor: &Conductor, file: &str, url: &str, doc: Document) {
        let meta = DocumentMeta {
            url: url.to_string(),
            stem: "test".to_string(),
            file: file.to_string(),
            page_kind: "item".to_string(),
        };
        let mut store = conductor.node_store.write().unwrap();
        document_to_store(&doc, &mut store, Some(&meta));
    }

    /// `content/test/doc.md`: title "Test Title"; summary "One", "Two"; body
    /// "Alpha and Alpha", "Beta". `content/test/other.md`: title "Other Title".
    fn make_conductor() -> (Conductor, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(
            root.join("schemas/test/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        add_doc(
            &conductor,
            FILE,
            "/test/doc",
            Document {
                preamble: im::vector![
                    title("Test Title"),
                    DocumentSlot {
                        name: SlotName::new("summary"),
                        elements: im::vector![p("One"), p("Two")],
                    },
                ],
                body: im::vector![p("Alpha and Alpha"), p("Beta")],
                has_separator: true,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
            },
        );
        add_doc(
            &conductor,
            OTHER,
            "/test/other",
            Document {
                preamble: im::vector![title("Other Title")],
                body: im::vector![],
                has_separator: false,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
            },
        );
        (conductor, tmp)
    }

    /// Texts of the blocks under the element selected by `sel`.
    fn block_texts(conductor: &Conductor, sel: &str) -> Vec<String> {
        let value = evaluator::eval_str_with_root(
            sel,
            &conductor.make_ned_query_root().expect("query root"),
        )
        .expect("selection evaluates");
        let sel = evaluator::ned_primitives::extract_selection(&value).expect("a selection");
        let store = conductor.node_store.read().unwrap();
        sel.iter()
            .flat_map(|container| store.children(container))
            .map(
                |block| match store.children(block).first().and_then(|t| store.get(*t)) {
                    Some(node_store::Node::Text(s)) => s.clone(),
                    other => panic!("expected a text child, got {other:?}"),
                },
            )
            .collect()
    }

    fn applied(result: CommandResult) -> usize {
        match result.response {
            Response::Applied { dirty_paths, .. } => dirty_paths,
            other => panic!("expected Response::Applied, got: {other:?}"),
        }
    }

    /// The refusal as a sentence: a plain error, or a refused plan's message.
    fn refused(result: CommandResult) -> String {
        match result.response {
            Response::Error(e) => e,
            Response::PlanRefused(failure) => failure.message,
            other => panic!("expected a refusal, got: {other:?}"),
        }
    }

    fn unchanged(conductor: &Conductor) {
        assert_eq!(
            block_texts(conductor, BODY),
            vec!["Alpha and Alpha", "Beta"]
        );
        assert_eq!(block_texts(conductor, SUMMARY), vec!["One", "Two"]);
        let title = block_texts(
            conductor,
            r#"(ned/slot (ned/doc-by-path "content/test/doc.md") "title")"#,
        );
        assert_eq!(title, vec!["Test Title"]);
    }

    const TABLE: &str = r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/test/tbl.md") "body")) "table" 0)"#;

    /// `content/test/tbl.md`: a body holding one table
    /// `| a | b |` / `| 1 | 2 |` / `| 3 | **4** |`.
    fn add_table_doc(conductor: &Conductor) {
        let table = content::parse_document("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | **4** |\n")
            .expect("parses")
            .elements[0]
            .node
            .clone();
        add_doc(
            conductor,
            "content/test/tbl.md",
            "/test/tbl",
            Document {
                preamble: im::vector![title("Table doc")],
                body: im::vector![spanned(table)],
                has_separator: true,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
            },
        );
    }

    /// The table of `content/test/tbl.md` as the store holds it.
    fn stored_table(conductor: &Conductor) -> ContentElement {
        let value = evaluator::eval_str_with_root(
            TABLE,
            &conductor.make_ned_query_root().expect("query root"),
        )
        .expect("selection evaluates");
        let sel = evaluator::ned_primitives::extract_selection(&value).expect("a selection");
        let id = sel.iter().next().expect("one table");
        let store = conductor.node_store.read().unwrap();
        node_store_bridge::content_bridge::block_from_node(&store, id).expect("decodes")
    }

    /// A cell is an inline container: its text is addressed like a heading's,
    /// and an edit changes exactly that cell (ADR-059 stage 6).
    #[test]
    fn table_cell_text_is_addressable() {
        let (conductor, _tmp) = make_conductor();
        add_table_doc(&conductor);
        let before = stored_table(&conductor);

        let program = format!(r#"(ned/set-text (ned/text-in (ned/cell {TABLE} 0 1)) "x")"#);
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);

        let flat =
            content::parse_document("| a | b |\n|---|---|\n| 1 | x |\n| 3 | **4** |\n").unwrap();
        assert_eq!(stored_table(&conductor), flat.elements[0].node.clone());
        assert_ne!(before, stored_table(&conductor));

        // The header cell, and a formatted cell's inner text.
        let program = format!(r#"(ned/set-text (ned/text-in (ned/header-cell {TABLE} 0)) "H")"#);
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        let program = format!(r#"(ned/set-text (ned/text-in (ned/cell {TABLE} 1 1)) "5")"#);
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        let flat =
            content::parse_document("| H | b |\n|---|---|\n| 1 | x |\n| 3 | **5** |\n").unwrap();
        assert_eq!(stored_table(&conductor), flat.elements[0].node.clone());
    }

    const QUOTE: &str = r#"(ned/nth-of-kind (ned/blocks (ned/slot (ned/doc-by-path "content/test/quote.md") "body")) "blockquote" 0)"#;

    /// `content/test/quote.md`: a body holding the quote `> a` / `> b` (two
    /// paragraphs).
    fn add_quote_doc(conductor: &Conductor) {
        let quote = content::parse_document("> a\n>\n> b\n")
            .expect("parses")
            .elements[0]
            .node
            .clone();
        add_doc(
            conductor,
            "content/test/quote.md",
            "/test/quote",
            Document {
                preamble: im::vector![title("Quote doc")],
                body: im::vector![spanned(quote)],
                has_separator: true,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
            },
        );
    }

    /// The quote of `content/test/quote.md` as the store holds it.
    fn stored_quote(conductor: &Conductor) -> ContentElement {
        let value = evaluator::eval_str_with_root(
            QUOTE,
            &conductor.make_ned_query_root().expect("query root"),
        )
        .expect("selection evaluates");
        let sel = evaluator::ned_primitives::extract_selection(&value).expect("a selection");
        let id = sel.iter().next().expect("one quote");
        let store = conductor.node_store.read().unwrap();
        node_store_bridge::content_bridge::block_from_node(&store, id).expect("decodes")
    }

    /// The blocks of a quote are its children (`ned/nth-child` of the quote,
    /// which `ned/blocks` also takes), and the text of one is addressed like any
    /// block's (ADR-059 stage 8.2).
    #[test]
    fn text_inside_blockquote_is_addressable() {
        let (conductor, _tmp) = make_conductor();
        add_quote_doc(&conductor);

        let program = format!(r#"(ned/set-text (ned/text-in (ned/nth-child {QUOTE} 1)) "x")"#);
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        let expected = content::parse_document("> a\n>\n> x\n").unwrap().elements[0]
            .node
            .clone();
        assert_eq!(stored_quote(&conductor), expected);

        // A block of the quote is a block target as well.
        let program = format!(
            r#"(ned/replace (ned/nth-child {QUOTE} 0) [(ned/with-child (ned/mk-element "paragraph") (ned/mk-text "y"))])"#
        );
        let result = conductor.apply_ned_program(&program);
        let expected = content::parse_document("> y\n>\n> x\n").unwrap().elements[0]
            .node
            .clone();
        assert_eq!(applied(result), 1);
        assert_eq!(stored_quote(&conductor), expected);
    }

    const LIST: &str = r#"(ned/nth-of-kind (ned/blocks (ned/slot (ned/doc-by-path "content/test/list.md") "body")) "list" 0)"#;

    /// `content/test/list.md`: a body holding the list `- a` / `- b`.
    fn add_list_doc(conductor: &Conductor) {
        let list = content::parse_document("- a\n- b\n")
            .expect("parses")
            .elements[0]
            .node
            .clone();
        add_doc(
            conductor,
            "content/test/list.md",
            "/test/list",
            Document {
                preamble: im::vector![title("List doc")],
                body: im::vector![spanned(list)],
                has_separator: true,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
            },
        );
    }

    /// The list of `content/test/list.md` as the store holds it.
    fn stored_list(conductor: &Conductor) -> ContentElement {
        let value = evaluator::eval_str_with_root(
            LIST,
            &conductor.make_ned_query_root().expect("query root"),
        )
        .expect("selection evaluates");
        let sel = evaluator::ned_primitives::extract_selection(&value).expect("a selection");
        let id = sel.iter().next().expect("one list");
        let store = conductor.node_store.read().unwrap();
        node_store_bridge::content_bridge::block_from_node(&store, id).expect("decodes")
    }

    /// The blocks of a list item are the children of the item (the item is the
    /// child of the list), and the text of one is addressed like any block's
    /// (ADR-059 stage 9.3).
    #[test]
    fn list_item_text_is_addressable() {
        let (conductor, _tmp) = make_conductor();
        add_list_doc(&conductor);

        let program = format!(
            r#"(ned/set-text (ned/text-in (ned/nth-child (ned/nth-child {LIST} 1) 0)) "x")"#
        );
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        let expected = content::parse_document("- a\n- x\n").unwrap().elements[0]
            .node
            .clone();
        assert_eq!(stored_list(&conductor), expected);

        // A block of the item is a block target as well.
        let program = format!(
            r#"(ned/replace (ned/nth-child (ned/nth-child {LIST} 0) 0) [(ned/with-child (ned/mk-element "paragraph") (ned/mk-text "y"))])"#
        );
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        let expected = content::parse_document("- y\n- x\n").unwrap().elements[0]
            .node
            .clone();
        assert_eq!(stored_list(&conductor), expected);
    }

    #[test]
    fn apply_program_with_ambiguous_selection_applies_nothing() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(r#"(ned/set-text (-> {SUMMARY} ned/descendants ned/texts) "X")"#);
        let e = refused(conductor.apply_ned_program(&program));
        assert!(e.starts_with("step 1 (ned/set-text):"), "{e}");
        assert!(e.contains("2 nodes"), "{e}");
        unchanged(&conductor);
    }

    #[test]
    fn apply_invalid_second_entry_applies_nothing() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(
            r#"(do (ned/set-text {TITLE_TEXT} "Changed") (ned/insert-child {BODY} (ned/mk-element "wibble")))"#
        );
        let e = refused(conductor.apply_ned_program(&program));
        assert!(e.starts_with("step 2 (ned/insert-child):"), "{e}");
        unchanged(&conductor);
    }

    #[test]
    fn apply_program_inserting_non_payload_element_is_refused() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(r#"(ned/insert-child {BODY} (ned/mk-element "wibble"))"#);
        let e = refused(conductor.apply_ned_program(&program));
        assert!(e.starts_with("step 1 (ned/insert-child):"), "{e}");
        unchanged(&conductor);
    }

    #[test]
    fn apply_search_replace_with_two_occurrences_is_refused() {
        let (conductor, _tmp) = make_conductor();
        let program =
            format!(r#"(ned/search-replace (ned/text-containing {BODY} "Alpha") "Alpha" "Gamma")"#);
        let e = refused(conductor.apply_ned_program(&program));
        assert!(e.starts_with("step 1 (ned/search-replace):"), "{e}");
        assert!(e.contains("2 times"), "{e}");
        unchanged(&conductor);
    }

    fn plan_refused(result: CommandResult) -> editorial_types::PlanFailure {
        match result.response {
            Response::PlanRefused(failure) => failure,
            other => panic!("expected Response::PlanRefused, got: {other:?}"),
        }
    }

    #[test]
    fn apply_refusal_names_step_origin_and_cause() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(
            r#"(do (ned/set-text {TITLE_TEXT} "x") (ned/set-text (-> {SUMMARY} ned/descendants ned/texts) "Y"))"#
        );
        let failure = plan_refused(conductor.apply_ned_program(&program));
        assert_eq!(failure.step, 2);
        assert_eq!(failure.origin, "ned/set-text");
        assert_eq!(
            failure.cause,
            editorial_types::StaleCause::Ambiguous { matches: 2 }
        );
        assert_eq!(
            failure.message,
            "step 2 (ned/set-text): the address matches 2 nodes; it must match exactly one"
        );
        unchanged(&conductor);
    }

    #[test]
    fn apply_overlap_refusal_names_the_earlier_step() {
        let (conductor, _tmp) = make_conductor();
        let program =
            format!(r#"(do (ned/set-text {TITLE_TEXT} "a") (ned/set-text {TITLE_TEXT} "b"))"#);
        let failure = plan_refused(conductor.apply_ned_program(&program));
        assert_eq!((failure.step, failure.origin.as_str()), (2, "ned/set-text"));
        assert_eq!(
            failure.cause,
            editorial_types::StaleCause::Overlap { with_step: 1 }
        );
        assert_eq!(
            failure.message,
            "step 2 (ned/set-text) changes what step 1 (ned/set-text) changes"
        );
        unchanged(&conductor);
    }

    /// An evaluation error is not a plan refusal: it stays a plain error.
    #[test]
    fn apply_eval_errors_stay_plain_errors() {
        let (conductor, _tmp) = make_conductor();
        let e = refused(conductor.apply_ned_program("(undefined-fn)"));
        assert!(e.starts_with("eval error:"), "{e}");
    }

    #[test]
    fn apply_eval_error_after_a_mutation_applies_nothing() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(r#"(do (ned/set-text {TITLE_TEXT} "x") (undefined-fn))"#);
        let e = refused(conductor.apply_ned_program(&program));
        assert!(e.starts_with("eval error:"), "{e}");
        unchanged(&conductor);
    }

    #[test]
    fn apply_query_only_program_dirties_nothing() {
        let (conductor, _tmp) = make_conductor();
        assert_eq!(
            applied(conductor.apply_ned_program(r#"(ned/doc-by-path "content/test/doc.md")"#)),
            0
        );
    }

    #[test]
    fn apply_delete_marks_its_document_dirty() {
        let (conductor, _tmp) = make_conductor();
        let program =
            format!(r#"(ned/delete (ned/nth-of-kind (ned/children {BODY}) "paragraph" 1))"#);
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        assert_eq!(block_texts(&conductor, BODY), vec!["Alpha and Alpha"]);
    }

    #[test]
    fn dirty_documents_come_from_the_plan() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(
            r#"(do (ned/set-text {TITLE_TEXT} "Changed") (ned/set-text {OTHER_TITLE_TEXT} "Other changed"))"#
        );
        assert_eq!(applied(conductor.apply_ned_program(&program)), 2);
    }

    #[test]
    fn apply_no_op_program_succeeds_with_nothing_dirty() {
        let (conductor, _tmp) = make_conductor();
        let program = format!(r#"(ned/set-text {TITLE_TEXT} "Test Title")"#);
        assert_eq!(applied(conductor.apply_ned_program(&program)), 0);
        unchanged(&conductor);
    }

    #[test]
    fn selection_cannot_see_nodes_inserted_earlier_in_the_program() {
        let (conductor, _tmp) = make_conductor();
        let new_paragraph = r#"(ned/with-child (ned/mk-element "paragraph") (ned/mk-text "New"))"#;
        // Step 2 addresses "the paragraph at index 1". Select-then-apply reads
        // the store as it was before the program: that is "Beta", not the
        // paragraph step 1 inserts.
        let program = format!(
            r#"(do
                 (ned/insert-after (ned/nth-of-kind (ned/children {BODY}) "paragraph" 0) {new_paragraph})
                 (ned/set-text (ned/text-in (ned/nth-of-kind (ned/children {BODY}) "paragraph" 1)) "Z"))"#
        );
        assert_eq!(applied(conductor.apply_ned_program(&program)), 1);
        assert_eq!(
            block_texts(&conductor, BODY),
            vec!["Alpha and Alpha", "New", "Z"]
        );
    }
}

// ---------------------------------------------------------------------------
// Browser body edits: the exact `bsave` program shape from inject.js, with a
// structural body selector (`(ned/slot doc "body")`-rooted).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod body_structural_bsave_tests {
    use super::*;
    use content::{ContentElement, Document, DocumentSlot};
    use node_store_bridge::content_bridge::{DocumentMeta, document_to_store, store_to_document};
    use schema::{HeadingLevel, SlotName, Span, Spanned};

    const SCHEMA_SRC: &str = "# Title {#title}\noccurs\n: exactly once\n";
    const FILE: &str = "content/test/doc.md";

    fn spanned(node: ContentElement) -> Spanned<ContentElement> {
        Spanned {
            node,
            span: Span { start: 0, end: 0 },
        }
    }

    fn h2(text: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::heading_from_markdown(
            HeadingLevel::new(2).unwrap(),
            text,
        ))
    }

    fn p(text: &str) -> Spanned<ContentElement> {
        spanned(ContentElement::paragraph_from_markdown(text))
    }

    /// A Clojure string literal for `s` (see `content::address::clj_str`).
    fn clj_str(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    }

    /// Conductor with `content/test/doc.md` whose body is:
    /// H2 "Intro", P "Intro para one", P "Intro para two", H2 "Why Presemble", P "Why para".
    /// Returns the conductor, its tempdir, and the document root NodeId.
    fn make_conductor_with_doc() -> (Conductor, tempfile::TempDir, node_store::NodeId) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/test")).expect("schemas");
        std::fs::create_dir_all(root.join("templates/test")).expect("templates");
        std::fs::create_dir_all(root.join("content/test")).expect("content");
        std::fs::write(root.join("schemas/test/item.md"), SCHEMA_SRC).expect("schema");
        std::fs::write(root.join("templates/test/item.hiccup"), "[:div]").expect("template");

        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let doc = Document {
            preamble: im::vector![DocumentSlot {
                name: SlotName::new("title"),
                elements: im::vector![spanned(ContentElement::heading_from_markdown(
                    HeadingLevel::new(1).unwrap(),
                    "Test Title",
                ))],
            }],
            body: im::vector![
                h2("Intro"),
                p("Intro para one"),
                p("Intro para two"),
                h2("Why Presemble"),
                p("Why para"),
            ],
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let meta = DocumentMeta {
            url: "/test/doc".to_string(),
            stem: "test".to_string(),
            file: FILE.to_string(),
            page_kind: "item".to_string(),
        };
        let doc_root = {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&doc, &mut store, Some(&meta))
        };

        (conductor, tmp, doc_root)
    }

    /// Build the program exactly as inject.js `bsave` does.
    fn bsave_program(sel: &str, bvalue: &str) -> String {
        format!(
            "(let [g (ned/parse-grammar {})]\n  (ned/replace\n    {}\n    (ned/parse-body {} g)))",
            clj_str(SCHEMA_SRC),
            sel,
            clj_str(bvalue)
        )
    }

    /// The markdown of a paragraph, as a test compares it.
    fn other_text(el: &ContentElement) -> String {
        el.text().map(|t| t.into_owned()).unwrap_or_default()
    }

    fn body_texts(conductor: &Conductor, doc_root: node_store::NodeId) -> Vec<(String, String)> {
        let store = conductor.node_store.read().unwrap();
        let doc = store_to_document(&store, doc_root).expect("store_to_document");
        doc.body
            .iter()
            .map(|el| match &el.node {
                ContentElement::Heading { content, .. } => (
                    "heading".to_string(),
                    content::AsPlainText::plain_text(content),
                ),
                ContentElement::Paragraph { .. } => ("paragraph".to_string(), other_text(&el.node)),
                other => ("other".to_string(), format!("{other:?}")),
            })
            .collect()
    }

    fn dirty_buffers(conductor: &Conductor) -> Vec<String> {
        match conductor.handle_command(Command::GetDirtyBuffers).response {
            Response::DirtyBuffers(paths) => paths,
            other => panic!("expected DirtyBuffers, got: {other:?}"),
        }
    }

    #[test]
    fn bsave_with_nth_after_heading_selector_replaces_body_paragraph_and_marks_dirty() {
        let (conductor, _tmp, doc_root) = make_conductor_with_doc();
        assert!(
            dirty_buffers(&conductor).is_empty(),
            "nothing dirty before the edit"
        );

        let sel = format!(
            "(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path {}) \"body\")) \"Intro\") \"paragraph\" 1)",
            clj_str(FILE)
        );
        let program = bsave_program(&sel, "Rewritten paragraph");

        match conductor
            .handle_command(Command::ApplyNedProgram { program })
            .response
        {
            Response::Applied { dirty_paths, .. } => {
                assert_eq!(
                    dirty_paths, 1,
                    "body edit should dirty exactly one document"
                );
            }
            Response::Error(e) => panic!("unexpected error: {e}"),
            other => panic!("expected Response::Applied, got: {other:?}"),
        }

        let body = body_texts(&conductor, doc_root);
        let expected: Vec<(String, String)> = [
            ("heading", "Intro"),
            ("paragraph", "Intro para one"),
            ("paragraph", "Rewritten paragraph"),
            ("heading", "Why Presemble"),
            ("paragraph", "Why para"),
        ]
        .iter()
        .map(|(k, t)| (k.to_string(), t.to_string()))
        .collect();
        assert_eq!(body, expected);

        assert_eq!(dirty_buffers(&conductor), vec![FILE.to_string()]);
    }

    #[test]
    fn bsave_with_heading_selector_replaces_body_heading_and_marks_dirty() {
        let (conductor, _tmp, doc_root) = make_conductor_with_doc();

        let sel = format!(
            "(ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path {}) \"body\")) \"Why Presemble\")",
            clj_str(FILE)
        );
        let program = bsave_program(&sel, "## Why \"Presemble\" rocks");

        match conductor
            .handle_command(Command::ApplyNedProgram { program })
            .response
        {
            Response::Applied { dirty_paths, .. } => assert_eq!(dirty_paths, 1),
            Response::Error(e) => panic!("unexpected error: {e}"),
            other => panic!("expected Response::Applied, got: {other:?}"),
        }

        let body = body_texts(&conductor, doc_root);
        assert_eq!(body.len(), 5, "body: {body:?}");
        assert_eq!(
            body[3],
            ("heading".to_string(), "Why \"Presemble\" rocks".to_string())
        );
        assert_eq!(body[4], ("paragraph".to_string(), "Why para".to_string()));
        assert_eq!(dirty_buffers(&conductor), vec![FILE.to_string()]);
    }

    /// Register `doc_root` under `path` for save routing and mark it dirty.
    fn register_dirty(conductor: &Conductor, path: &str, doc_root: node_store::NodeId) {
        conductor
            .path_to_doc_root
            .write()
            .unwrap()
            .insert(PathBuf::from(path), doc_root);
        conductor
            .dirty_docs
            .write()
            .unwrap()
            .mark(PathBuf::from(path));
    }

    /// Register the doc for save routing, mark it dirty, and delete its body.
    /// Schema-invalid, but still well-formed — it must save (ADR-057 §4).
    fn dirty_doc_without_body(conductor: &Conductor, doc_root: node_store::NodeId) {
        register_dirty(conductor, FILE, doc_root);
        let mut store = conductor.node_store.write().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&store, doc_root, "body")
            .unwrap();
        store.remove_child_edge(doc_root, body);
    }

    /// Register the doc for save routing, mark it dirty, and add a malformed
    /// block (unknown element kind `wibble`) under its body.
    fn dirty_doc_with_malformed_block(conductor: &Conductor, doc_root: node_store::NodeId) {
        register_dirty(conductor, FILE, doc_root);
        let mut store = conductor.node_store.write().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&store, doc_root, "body")
            .unwrap();
        let name = store.intern("wibble");
        let wibble = store.add_node(node_store::Node::Element(name));
        store.add_edge(body, node_store::Edge::Child(wibble));
    }

    /// Register a second, well-formed dirty document at `path`.
    fn add_good_dirty_doc(conductor: &Conductor, path: &str, url: &str) {
        let good_doc = Document {
            preamble: im::vector![DocumentSlot {
                name: SlotName::new("title"),
                elements: im::vector![spanned(ContentElement::heading_from_markdown(
                    HeadingLevel::new(1).unwrap(),
                    "Other Title",
                ))],
            }],
            body: im::vector![p("Other para")],
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let good_meta = DocumentMeta {
            url: url.to_string(),
            stem: "test".to_string(),
            file: path.to_string(),
            page_kind: "item".to_string(),
        };
        let good_root = {
            let mut store = conductor.node_store.write().unwrap();
            document_to_store(&good_doc, &mut store, Some(&good_meta))
        };
        register_dirty(conductor, path, good_root);
    }

    #[test]
    fn save_buffer_with_deleted_body_saves_preamble_only() {
        let (conductor, tmp, doc_root) = make_conductor_with_doc();
        dirty_doc_without_body(&conductor, doc_root);

        let response = conductor
            .handle_command(Command::SaveBuffer {
                path: editorial_types::SitePath::parse(FILE).expect("valid"),
                consent: Default::default(),
            })
            .response;
        assert!(
            !matches!(response, Response::Error(_)),
            "schema-invalid doc must save: {response:?}"
        );
        let written = std::fs::read_to_string(tmp.path().join(FILE)).expect("doc written");
        assert!(written.contains("Test Title"), "written: {written}");
        assert!(!written.contains("Intro para one"), "written: {written}");
        assert!(
            dirty_buffers(&conductor).is_empty(),
            "doc is clean after save"
        );
    }

    #[test]
    fn save_all_buffers_with_deleted_body_saves_preamble_only() {
        let (conductor, tmp, doc_root) = make_conductor_with_doc();
        dirty_doc_without_body(&conductor, doc_root);

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: Default::default(),
            })
            .response;
        assert!(
            !matches!(response, Response::Error(_)),
            "schema-invalid doc must save: {response:?}"
        );
        let written = std::fs::read_to_string(tmp.path().join(FILE)).expect("doc written");
        assert!(written.contains("Test Title"), "written: {written}");
        assert!(!written.contains("Intro para one"), "written: {written}");
        assert!(
            dirty_buffers(&conductor).is_empty(),
            "doc is clean after save"
        );
    }

    #[test]
    fn save_all_buffers_saves_good_doc_and_keeps_broken_doc_dirty() {
        // Sorts after FILE so the broken doc is visited first.
        const GOOD: &str = "content/test/other.md";

        let (conductor, tmp, broken_root) = make_conductor_with_doc();
        dirty_doc_with_malformed_block(&conductor, broken_root);
        add_good_dirty_doc(&conductor, GOOD, "/test/other");

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: Default::default(),
            })
            .response;
        match response {
            Response::Error(e) => {
                assert!(e.contains(FILE), "error must name the broken doc: {e}");
                assert!(
                    e.contains("wibble"),
                    "error must name the malformed block: {e}"
                );
                assert!(!e.contains(GOOD), "error must not name the good doc: {e}");
            }
            other => panic!("expected Response::Error, got: {other:?}"),
        }

        let written = std::fs::read_to_string(tmp.path().join(GOOD)).expect("good doc written");
        assert!(written.contains("Other para"), "written: {written}");
        assert!(
            !tmp.path().join(FILE).exists(),
            "broken doc must not be written"
        );
        assert_eq!(
            dirty_buffers(&conductor),
            vec![FILE.to_string()],
            "only the broken doc stays dirty"
        );
    }

    #[test]
    fn save_all_buffers_write_failure_keeps_doc_dirty_and_saves_the_rest() {
        // Sorts after FILE so the failing doc is visited first.
        const GOOD: &str = "content/test/other.md";

        let (conductor, tmp, doc_root) = make_conductor_with_doc();
        register_dirty(&conductor, FILE, doc_root);
        add_good_dirty_doc(&conductor, GOOD, "/test/other");
        // A directory at the target path makes the write fail.
        std::fs::create_dir_all(tmp.path().join(FILE)).expect("blocking dir");

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: Default::default(),
            })
            .response;
        match response {
            Response::Error(e) => {
                assert!(e.contains(FILE), "error must name the failed doc: {e}");
                assert!(!e.contains(GOOD), "error must not name the good doc: {e}");
            }
            other => panic!("expected Response::Error, got: {other:?}"),
        }

        let written = std::fs::read_to_string(tmp.path().join(GOOD)).expect("good doc written");
        assert!(written.contains("Other para"), "written: {written}");
        assert!(tmp.path().join(FILE).is_dir(), "blocking dir untouched");
        assert_eq!(
            dirty_buffers(&conductor),
            vec![FILE.to_string()],
            "only the failed doc stays dirty"
        );
    }

    #[test]
    fn save_all_buffers_reports_dirty_doc_missing_from_store() {
        let (conductor, tmp, _doc_root) = make_conductor_with_doc();
        // Dirty, but never registered in path_to_doc_root.
        conductor
            .dirty_docs
            .write()
            .unwrap()
            .mark(PathBuf::from(FILE));

        let response = conductor
            .handle_command(Command::SaveAllBuffers {
                consent: Default::default(),
            })
            .response;
        match response {
            Response::Error(e) => assert!(e.contains(FILE), "error must name the doc: {e}"),
            other => panic!("expected Response::Error, got: {other:?}"),
        }
        assert!(!tmp.path().join(FILE).exists(), "nothing must be written");
        assert_eq!(
            dirty_buffers(&conductor),
            vec![FILE.to_string()],
            "doc stays dirty"
        );
    }
}

// ---------------------------------------------------------------------------
// Tests for schema_url_for_page and page_url_for_schema
// ---------------------------------------------------------------------------

#[cfg(test)]
mod schema_url_routing_tests {
    use super::*;

    /// Build a site with:
    ///   - Root index (`content/index.md`, stem="", page-kind="collection", url="/")
    ///   - Post collection (`content/post/index.md`, stem="post", page-kind="collection", url="/post/")
    ///   - Two post items (`content/post/foo.md` url="/post/foo", `content/post/bar.md` url="/post/bar")
    fn build_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // schemas
        std::fs::create_dir_all(root.join("schemas/post")).expect("schemas/post");
        let item_schema = "# Title {#title}\noccurs\n: exactly once\n";
        std::fs::write(root.join("schemas/post/item.md"), item_schema).expect("item schema");
        std::fs::write(root.join("schemas/post/index.md"), item_schema).expect("collection schema");

        // Root index schema (for the root page to appear in NodeStore)
        std::fs::write(root.join("schemas/index.md"), item_schema).expect("root schema");

        // templates (minimal so pages can build)
        std::fs::create_dir_all(root.join("templates/post")).expect("templates/post");
        let tmpl = "[:div [:h1 title]]";
        std::fs::write(root.join("templates/post/item.hiccup"), tmpl).expect("item template");
        std::fs::write(root.join("templates/post/index.hiccup"), tmpl)
            .expect("collection template");
        std::fs::write(root.join("templates/index.hiccup"), tmpl).expect("root template");

        // content
        std::fs::create_dir_all(root.join("content/post")).expect("content/post");
        let content = "title: Hello\n---\nBody\n";
        std::fs::write(root.join("content/index.md"), content).expect("root index");
        std::fs::write(root.join("content/post/index.md"), content).expect("post index");
        std::fs::write(root.join("content/post/foo.md"), content).expect("foo");
        std::fs::write(root.join("content/post/bar.md"), content).expect("bar");

        tmp
    }

    fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    // ── schema_url_for_page tests ────────────────────────────────────────────

    #[test]
    fn schema_url_for_page_root_index() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/"),
            Some("/_schema/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_collection() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/post/"),
            Some("/_schema/post/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_item() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/post/foo"),
            Some("/_schema/post/item".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_canonicalizes_index_html() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // /post/foo/index.html → strips index.html → /post/foo/ → tries /post/foo/ then /post/foo
        assert_eq!(
            conductor.schema_url_for_page("/post/foo/index.html"),
            Some("/_schema/post/item".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_unknown_returns_none() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(conductor.schema_url_for_page("/no/such/page"), None);
    }

    #[test]
    fn schema_url_for_page_canonicalizes_root_index_html() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/index.html"),
            Some("/_schema/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_canonicalizes_collection_index_html() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.schema_url_for_page("/post/index.html"),
            Some("/_schema/post/index".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_handles_item_with_trailing_slash() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // The test fixture has /post/foo as an item URL (no trailing slash stored)
        // but the canonicalization strips trailing slash, so /post/foo/ → /post/foo
        assert_eq!(
            conductor.schema_url_for_page("/post/foo/"),
            Some("/_schema/post/item".to_string())
        );
    }

    #[test]
    fn schema_url_for_page_handles_collection_without_trailing_slash() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // /post (no trailing slash) should resolve to the collection page
        assert_eq!(
            conductor.schema_url_for_page("/post"),
            Some("/_schema/post/index".to_string())
        );
    }

    // ── page_url_for_schema tests ────────────────────────────────────────────

    #[test]
    fn page_url_for_schema_root_index() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.page_url_for_schema("/_schema/index"),
            Some("/".to_string())
        );
    }

    #[test]
    fn page_url_for_schema_collection() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.page_url_for_schema("/_schema/post/index"),
            Some("/post/".to_string())
        );
    }

    #[test]
    fn page_url_for_schema_item_prefers_collection_page() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        // Fixture has /post/ collection page — that should be returned first
        assert_eq!(
            conductor.page_url_for_schema("/_schema/post/item"),
            Some("/post/".to_string())
        );
    }

    #[test]
    fn page_url_for_schema_item_falls_back_to_first_item_when_no_collection() {
        // Build a site WITHOUT a collection index page for "post"
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        std::fs::create_dir_all(root.join("schemas/post")).expect("schemas/post");
        let item_schema = "# Title {#title}\noccurs\n: exactly once\n";
        std::fs::write(root.join("schemas/post/item.md"), item_schema).expect("item schema");
        // No schemas/post/index.md  (no collection schema)
        std::fs::write(root.join("schemas/index.md"), item_schema).expect("root schema");

        std::fs::create_dir_all(root.join("templates/post")).expect("templates/post");
        let tmpl = "[:div [:h1 title]]";
        std::fs::write(root.join("templates/post/item.hiccup"), tmpl).expect("item template");
        std::fs::write(root.join("templates/index.hiccup"), tmpl).expect("root template");

        std::fs::create_dir_all(root.join("content/post")).expect("content/post");
        let content = "title: Hello\n---\nBody\n";
        std::fs::write(root.join("content/index.md"), content).expect("root index");
        // No content/post/index.md — no collection page
        std::fs::write(root.join("content/post/foo.md"), content).expect("foo");
        std::fs::write(root.join("content/post/bar.md"), content).expect("bar");

        let conductor = make_conductor(&tmp);

        // No collection page exists for "post", so falls back to first item (/post/bar sorts first)
        let result = conductor.page_url_for_schema("/_schema/post/item");
        assert!(
            result.is_some(),
            "expected Some(url) for known item schema without collection page"
        );
        let url = result.unwrap();
        assert!(
            url.starts_with("/post/"),
            "result should be under /post/, got: {url}"
        );
    }

    #[test]
    fn collection_url_is_canonical_for_root_and_named_stems() {
        // Root stem's collection URL is "/", never "//"; named stems get "/{stem}/".
        assert_eq!(collection_url(""), "/");
        assert_eq!(collection_url("post"), "/post/");
    }

    #[test]
    fn page_url_for_schema_unknown_stem_returns_none() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            conductor.page_url_for_schema("/_schema/no-such-stem/item"),
            None
        );
    }

    #[test]
    fn page_url_for_schema_malformed_returns_none() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(conductor.page_url_for_schema("/_schema/foo/bar/baz"), None);
    }
}

// ---------------------------------------------------------------------------
// Tests for body_idx_to_structural
// ---------------------------------------------------------------------------

#[cfg(test)]
mod body_idx_to_structural_tests {
    use super::*;

    /// Build a minimal Document from source text using a simple schema that allows a body.
    fn parse_doc(src: &str) -> content::Document {
        // Schema with a body section
        let schema_src = "# Title {#title}\noccurs\n: exactly once\n\n----\nBody.\n";
        let grammar = schema::parse_schema(schema_src).expect("parse schema");
        content::parse_and_assign(src, &grammar).expect("parse doc")
    }

    #[test]
    fn body_idx_to_structural_no_preceding_heading() {
        // Body: p0, p1, p2 — no heading before any of them
        let src = "# Title\n\n----\n\nFirst.\n\nSecond.\n\nThird.\n";
        let doc = parse_doc(src);
        let anchor =
            body_idx_to_structural(&doc, "content/post/x.md", 1).expect("should produce anchor");
        assert_eq!(anchor.slot, "body");
        assert_eq!(anchor.node_kind, "paragraph");
        assert_eq!(anchor.heading_text, None);
        assert_eq!(
            anchor.offset, 1,
            "second paragraph with no heading has offset 1"
        );
        assert_eq!(anchor.file, "content/post/x.md");
    }

    #[test]
    fn body_idx_to_structural_with_preceding_heading() {
        // Body: ## Intro, p0, p1
        // body[0] = heading "Intro", body[1] = p0, body[2] = p1
        let src = "# Title\n\n----\n\n## Intro\n\nFirst para.\n\nSecond para.\n";
        let doc = parse_doc(src);
        // body[2] = "Second para." — heading_text = "Intro", offset = 1
        let anchor =
            body_idx_to_structural(&doc, "content/post/x.md", 2).expect("should produce anchor");
        assert_eq!(anchor.node_kind, "paragraph");
        assert_eq!(anchor.heading_text, Some("Intro".to_string()));
        assert_eq!(
            anchor.offset, 1,
            "second paragraph after heading has offset 1"
        );
    }

    #[test]
    fn body_idx_to_structural_kind_dispatch_counts_only_matching_kind() {
        // Body: ## Heading, p0, p1, h2, p2
        // body[0] = ## Heading, body[1] = p0, body[2] = p1, body[3] = ## Another, body[4] = p2
        let src = "# Title\n\n----\n\n## Heading\n\nFirst para.\n\nSecond para.\n\n## Another\n\nThird para.\n";
        let doc = parse_doc(src);
        // body[2] = "Second para." → after heading "Heading", offset among paragraphs = 1
        let anchor =
            body_idx_to_structural(&doc, "content/post/x.md", 2).expect("should produce anchor");
        assert_eq!(anchor.node_kind, "paragraph");
        assert_eq!(anchor.heading_text, Some("Heading".to_string()));
        assert_eq!(
            anchor.offset, 1,
            "second paragraph after heading has offset 1 (heading not counted)"
        );

        // body[3] = ## Another → this is a heading, heading_text from body[0], offset 0
        let anchor_h = body_idx_to_structural(&doc, "content/post/x.md", 3)
            .expect("should produce anchor for heading");
        assert_eq!(anchor_h.node_kind, "heading");
        assert_eq!(anchor_h.heading_text, Some("Heading".to_string()));
        assert_eq!(
            anchor_h.offset, 0,
            "first heading after heading has offset 0"
        );
    }

    #[test]
    fn body_idx_to_structural_normalises_heading_whitespace() {
        // Heading with extra internal whitespace
        let src = "# Title\n\n----\n\n## Why  Presemble\n\nA paragraph.\n";
        let doc = parse_doc(src);
        // body[1] = paragraph, heading_text should be normalised
        let anchor =
            body_idx_to_structural(&doc, "content/post/x.md", 1).expect("should produce anchor");
        assert_eq!(
            anchor.heading_text,
            Some("Why Presemble".to_string()),
            "extra whitespace should be collapsed"
        );
    }

    /// The anchor's heading text is the heading's plain text, as rendered
    /// (ADR-063): markup, escapes, images and comments are not part of it.
    #[test]
    fn body_idx_to_structural_uses_plain_heading_text() {
        let src =
            "# Title\n\n----\n\n## ![i](x.png) The `<div>` *element* <!-- c -->\n\nA paragraph.\n";
        let doc = parse_doc(src);
        let anchor =
            body_idx_to_structural(&doc, "content/post/x.md", 1).expect("should produce anchor");
        assert_eq!(anchor.heading_text, Some("The <div> element".to_string()));
    }

    #[test]
    fn body_idx_to_structural_out_of_range_returns_none() {
        let src = "# Title\n\n----\n\nOnly paragraph.\n";
        let doc = parse_doc(src);
        assert!(
            body_idx_to_structural(&doc, "content/post/x.md", 99).is_none(),
            "out-of-range index should return None"
        );
    }
}

/// Package G, step 0: link-expression operations take effect in conductor
/// builds. Fixture: three posts, two authors, one `page/home`.
#[cfg(test)]
mod link_expression_pipeline_tests {
    use super::*;

    const POST_SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\n\n\
        Published date. {#published}\noccurs\n: exactly once\n\n\
        Category. {#category}\noccurs\n: exactly once\n\n\
        [<name>](/author/<name>) {#author}\noccurs\n: exactly once\ntype\n: link(author)\n\n\
        ----\n\nBody.\n";
    const AUTHOR_SCHEMA: &str = "# Author name {#name}\noccurs\n: exactly once\n\n\
        [<name>](/post/<name>) {#posts}\noccurs\n: *\ntype\n: link(post)\n\n\
        ----\n\nBody.\n";
    const PAGE_SCHEMA: &str = "# Page title {#title}\noccurs\n: exactly once\n\n\
        [<name>](/post/<name>) {#latest}\noccurs\n: *\ntype\n: link(post)\n\n\
        [<name>](/post/<name>) {#news}\noccurs\n: *\ntype\n: link(post)\n\n\
        ----\n\nBody.\n";

    fn post(title: &str, author: &str, published: &str, category: &str) -> String {
        format!(
            "# {title}\n\n{published}\n\n{category}\n\n[{author}](/author/{author})\n\n----\n\nBody of {title}.\n"
        )
    }

    const AUTHOR_A: &str =
        "# Author A\n\n[Posts](->> :post (refs-to self)) {#posts}\n\n----\n\nAbout A.\n";
    const AUTHOR_B: &str =
        "# Author B\n\n[Posts](->> :post (refs-to self)) {#posts}\n\n----\n\nAbout B.\n";
    const HOME: &str = "# Home\n\n\
        [Latest posts](->> :post (sort-by :published :desc) (take 2)) {#latest}\n\n\
        [In the news](->> :post (filter :category \"news\")) {#news}\n\n----\n\nWelcome.\n";

    pub(super) fn build_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for stem in ["post", "author", "page"] {
            std::fs::create_dir_all(root.join(format!("schemas/{stem}"))).unwrap();
            std::fs::create_dir_all(root.join(format!("templates/{stem}"))).unwrap();
            std::fs::create_dir_all(root.join(format!("content/{stem}"))).unwrap();
            std::fs::write(root.join(format!("templates/{stem}/item.hiccup")), "[:div]").unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), POST_SCHEMA).unwrap();
        std::fs::write(root.join("schemas/author/item.md"), AUTHOR_SCHEMA).unwrap();
        std::fs::write(root.join("schemas/page/item.md"), PAGE_SCHEMA).unwrap();
        std::fs::write(
            root.join("content/post/p1.md"),
            post("Post one", "a", "2026-01-10", "notes"),
        )
        .unwrap();
        std::fs::write(
            root.join("content/post/p2.md"),
            post("Post two", "b", "2026-02-10", "news"),
        )
        .unwrap();
        std::fs::write(
            root.join("content/post/p3.md"),
            post("Post three", "a", "2026-03-10", "news"),
        )
        .unwrap();
        std::fs::write(root.join("content/author/a.md"), AUTHOR_A).unwrap();
        std::fs::write(root.join("content/author/b.md"), AUTHOR_B).unwrap();
        std::fs::write(root.join("content/page/home.md"), HOME).unwrap();
        tmp
    }

    pub(super) fn make_conductor(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    /// The URLs held by semantic slot `slot` of `page`, in order.
    pub(super) fn slot_urls(conductor: &Conductor, page: &str, slot: &str) -> Vec<String> {
        let sem = conductor
            .semantic_content_by_url(page)
            .unwrap_or_else(|| panic!("no sem for {page}"));
        let store = conductor.node_store.read().unwrap();
        let url_of = |id| {
            node_store_bridge::content_bridge::find_attr_text(&store, id, "url").unwrap_or_default()
        };
        let mut out = Vec::new();
        for (name, target) in store
            .consists_of(sem)
            .into_iter()
            .chain(store.references(sem))
        {
            if store.resolve_name(name) != slot {
                continue;
            }
            if matches!(store.get(target), Some(node_store::Node::Collection)) {
                out.extend(store.children(target).into_iter().map(url_of));
            } else {
                out.push(url_of(target));
            }
        }
        out
    }

    fn urls(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn full_build_applies_link_expression_operations() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3", "/post/p2"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "news"),
            urls(&["/post/p2", "/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/b", "posts"),
            urls(&["/post/p2"])
        );
    }

    /// Guard: a thread sorts by a title's plain text, not by its markdown
    /// source: `*Zebra*` sorts as `Zebra`, after `Apple` and `Mango`.
    #[test]
    fn sorting_by_a_formatted_title_uses_its_plain_text() {
        let tmp = build_site();
        let root = tmp.path();
        for (slug, title) in [("p1", "*Zebra*"), ("p2", "Apple"), ("p3", "Mango")] {
            std::fs::write(
                root.join(format!("content/post/{slug}.md")),
                post(title, "a", "2026-01-10", "notes"),
            )
            .unwrap();
        }
        std::fs::write(
            root.join("content/page/home.md"),
            "# Home\n\n[By title](->> :post (sort-by :title)) {#latest}\n\n----\n\nWelcome.\n",
        )
        .unwrap();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p2", "/post/p3", "/post/p1"])
        );
    }

    /// A collection page that lists posts through a thread slot shows each
    /// post's title with its formatting: `Hello *World*` is `Hello <em>World</em>`.
    #[test]
    fn collection_listing_shows_a_formatted_title_with_its_formatting() {
        let tmp = build_site();
        let root = tmp.path();
        std::fs::write(
            root.join("content/post/p3.md"),
            post("Hello *World*", "a", "2026-03-10", "news"),
        )
        .unwrap();
        std::fs::write(
            root.join("content/page/home.md"),
            "# Home\n\n[Latest post](->> :post (sort-by :published :desc) (take 1)) {#latest}\n\n----\n\nWelcome.\n",
        )
        .unwrap();
        std::fs::remove_file(root.join("templates/page/item.hiccup")).unwrap();
        std::fs::write(
            root.join("templates/page/item.html"),
            r#"<ul><template data-each="(ref input.latest)"><li><presemble:insert data="(ref item.title)" as="h3" /></li></template></ul>"#,
        )
        .unwrap();
        let conductor = make_conductor(&tmp);
        conductor.render_url("/page/home").expect("renders");
        let html = std::fs::read_to_string(site_index::output_path_for_stem_slug(
            &conductor.output_dir,
            "page",
            "home",
        ))
        .expect("the page is rendered");
        assert!(html.contains("Hello <em>World</em></h3>"), "{html}");
    }

    fn apply_edn_block_edit(
        conductor: &Conductor,
        file: &str,
        selection: &str,
        base: editorial_types::BlockText,
        proposed: editorial_types::BlockText,
    ) {
        let result =
            conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
                file: editorial_types::ContentPath::parse(file).expect("valid"),
                selection: selection.to_string(),
                base,
                proposed,
                intent: editorial_types::BlockEditIntent::Apply,
                reason: None,
            }));
        assert!(
            matches!(
                result.response,
                Response::BlockEdited(editorial_types::BlockEditResponse::Done(
                    editorial_types::BlockEditOutcome::Applied { .. }
                ))
            ),
            "{:?}",
            result.response
        );
    }

    /// A structural edit refreshes the page's references (`fill_semantic_content`
    /// builds a thread slot without a thread context, listing every post), and
    /// the thread slots are evaluated again after it: the listing keeps its
    /// sort and its limit, on the page that holds it and on a page it lists.
    #[test]
    fn structural_edit_on_a_page_with_a_thread_slot_keeps_its_listing() {
        let tmp = build_site();
        std::fs::write(
            tmp.path().join("content/page/home.md"),
            "# Home\n\n[Latest post](->> :post (sort-by :published :desc) (take 1)) {#latest}\n\n\
             [In the news](->> :post (filter :category \"news\")) {#news}\n\n----\n\nWelcome.\n",
        )
        .unwrap();
        let conductor = make_conductor(&tmp);
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3"])
        );

        // A Replace of a body block on the page that holds the listing.
        apply_edn_block_edit(
            &conductor,
            "content/page/home.md",
            r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "content/page/home.md"))) "paragraph" 0)"#,
            editorial_types::BlockText::Markdown("Welcome.".to_string()),
            editorial_types::BlockText::Markdown("Welcome.\n\nAgain.".to_string()),
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "news"),
            urls(&["/post/p2", "/post/p3"])
        );

        // A Replace of the title of a post that the listing shows.
        apply_edn_block_edit(
            &conductor,
            "content/post/p3.md",
            r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/p3.md") "title")) "heading" 0)"#,
            editorial_types::BlockText::Edn("[:heading {:level \"1\"} \"Post three\"]".to_string()),
            editorial_types::BlockText::Edn("[:heading {:level \"2\"} \"Third post\"]".to_string()),
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "news"),
            urls(&["/post/p2", "/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3"])
        );
    }

    /// Pin: how the two PathRef spellings read back, and whether they match
    /// the `url_to_semantic` key `/author/a` without and with a leading-slash
    /// normalisation applied to both sides.
    #[test]
    fn path_ref_spellings_against_url_to_semantic() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let normalise = |u: &str| {
            if u.starts_with('/') {
                u.to_string()
            } else {
                format!("/{u}")
            }
        };
        let keys: Vec<String> = conductor
            .url_to_semantic
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        for (src, expected_value) in [
            ("[](:author/a)", "author/a"),
            ("[](:/author/a)", "/author/a"),
        ] {
            let flat = content::parse_document(src).expect("parse");
            let value = match &flat.elements[0].node {
                content::ContentElement::LinkExpression {
                    target: content::LinkTarget::PathRef(v),
                    ..
                } => v.clone(),
                other => panic!("{src}: expected PathRef, got {other:?}"),
            };
            assert_eq!(value, expected_value, "{src}");
            let raw_match = keys.contains(&value);
            let normalised_match = keys.iter().any(|k| normalise(k) == normalise(&value));
            eprintln!(
                "PIN {src}: value={value:?} raw_match={raw_match} normalised_match={normalised_match}"
            );
            assert!(
                normalised_match,
                "{src} must match /author/a after normalisation"
            );
        }
    }

    #[test]
    fn page_slots_are_assigned_separately() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let root = conductor.document_by_url("/page/home").expect("home");
        let store = conductor.node_store.read().unwrap();
        let doc = node_store_bridge::content_bridge::store_to_document(&store, root).unwrap();
        let counts: Vec<(String, usize)> = doc
            .preamble
            .iter()
            .map(|s| (s.name.as_str().to_string(), s.elements.len()))
            .collect();
        assert_eq!(
            counts,
            vec![
                ("title".to_string(), 1),
                ("latest".to_string(), 1),
                ("news".to_string(), 1)
            ],
            "each link expression must land in its own slot"
        );
    }

    // ── Helpers for the pins and incremental tests ───────────────────────────

    use std::collections::BTreeSet;

    fn write(tmp: &tempfile::TempDir, rel: &str, text: &str) {
        std::fs::write(tmp.path().join(rel), text).unwrap();
    }

    /// A post whose author slot is replaced by `author_lines` (verbatim).
    fn post_with(title: &str, published: &str, category: &str, author_lines: &str) -> String {
        format!(
            "# {title}\n\n{published}\n\n{category}\n\n{author_lines}\n\n----\n\nBody of {title}.\n"
        )
    }

    fn set(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn pages_of(result: &CommandResult) -> BTreeSet<String> {
        result
            .events
            .iter()
            .flat_map(|ev| match ev {
                ConductorEvent::PagesRebuilt { pages, .. } => pages.clone(),
                _ => vec![],
            })
            .collect()
    }

    /// The pages re-rendered when the document `rel` is read again from `text`
    /// (the fallback of an editor change, and a change on disk): the page
    /// rebuild these tests pin.
    fn document_changed(
        conductor: &Conductor,
        _tmp: &tempfile::TempDir,
        rel: &str,
        text: &str,
    ) -> BTreeSet<String> {
        let site_path = editorial_types::SitePath::parse(rel).expect("valid");
        let abs = crate::site_resolve::resolve_in_site(&conductor.site_dir, &site_path)
            .expect("inside the site");
        conductor
            .rebuild_page(&abs, text)
            .unwrap_or_else(|e| panic!("build failed: {e}"))
            .into_iter()
            .collect()
    }

    fn count(list: &[String], url: &str) -> usize {
        list.iter().filter(|u| *u == url).count()
    }

    /// The unassigned elements of the page at `url`.
    fn unassigned(conductor: &Conductor, url: &str) -> Vec<content::ContentElement> {
        let root = conductor
            .document_by_url(url)
            .unwrap_or_else(|| panic!("no doc {url}"));
        let store = conductor.node_store.read().unwrap();
        node_store_bridge::content_bridge::store_to_document(&store, root)
            .unwrap()
            .unassigned
            .iter()
            .map(|s| s.node.clone())
            .collect()
    }

    // ── U1–U5: every declared reference counts ───────────────────────────────

    /// U1: p5 links `/author/a` from its author slot and also has an
    /// unassigned paragraph (an Error). No publishability filter applies.
    #[test]
    fn u1_unpublishable_post_counts_for_refs_to() {
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p5.md",
            &post_with(
                "Post five",
                "2025-06-01",
                "notes",
                "[a](/author/a)\n\nStray paragraph.",
            ),
        );
        let conductor = make_conductor(&tmp);
        assert_eq!(
            unassigned(&conductor, "/post/p5"),
            vec![content::ContentElement::paragraph_from_markdown(
                "Stray paragraph."
            )]
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3", "/post/p5"])
        );
    }

    /// U2: p6's only link to `/author/a` is an unassigned PathRef whose
    /// marker binds no slot.
    #[test]
    fn u2_unassigned_bad_marker_path_ref_counts() {
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p6.md",
            &post_with("Post six", "2025-06-02", "notes", "[A](:author/a) {#nope}"),
        );
        let conductor = make_conductor(&tmp);
        assert!(
            matches!(
                unassigned(&conductor, "/post/p6").as_slice(),
                [content::ContentElement::LinkExpression {
                    target: content::LinkTarget::PathRef(_),
                    ..
                }]
            ),
            "{:?}",
            unassigned(&conductor, "/post/p6")
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3", "/post/p6"])
        );
    }

    /// U3: p7's only link is an unassigned marker-shaped paragraph.
    #[test]
    fn u3_unassigned_marker_shaped_paragraph_counts() {
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p7.md",
            &post_with(
                "Post seven",
                "2025-12-01",
                "notes",
                "[A](/author/a) {#nope}",
            ),
        );
        let conductor = make_conductor(&tmp);
        assert_eq!(
            unassigned(&conductor, "/post/p7"),
            vec![content::ContentElement::paragraph_from_markdown(
                "[A](/author/a) {#nope}"
            )]
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3", "/post/p7"])
        );
    }

    /// U4: after p6 and p7 are fixed (the link moved into the author slot),
    /// each appears exactly once.
    #[test]
    fn u4_fixed_posts_appear_exactly_once() {
        let tmp = build_site();
        // p6: the marked PathRef now names the author slot.
        write(
            &tmp,
            "content/post/p6.md",
            &post_with(
                "Post six",
                "2025-06-02",
                "notes",
                "[A](:author/a) {#author}",
            ),
        );
        // p7: a plain link fills the author slot by position (plain links
        // take no marker).
        write(
            &tmp,
            "content/post/p7.md",
            &post_with("Post seven", "2025-12-01", "notes", "[A](/author/a)"),
        );
        let conductor = make_conductor(&tmp);
        assert!(unassigned(&conductor, "/post/p6").is_empty());
        assert!(unassigned(&conductor, "/post/p7").is_empty());
        let posts = slot_urls(&conductor, "/author/a", "posts");
        assert_eq!(
            posts,
            urls(&["/post/p1", "/post/p3", "/post/p6", "/post/p7"])
        );
        assert_eq!(count(&posts, "/post/p6"), 1);
        assert_eq!(count(&posts, "/post/p7"), 1);
    }

    /// U5: p8's only link to `/author/a` is in an ordinary unassigned
    /// paragraph.
    #[test]
    fn u5_link_in_ordinary_unassigned_paragraph_counts() {
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p8.md",
            &post_with(
                "Post eight",
                "2025-06-03",
                "notes",
                "See [A](/author/a) for more.",
            ),
        );
        let conductor = make_conductor(&tmp);
        assert_eq!(
            unassigned(&conductor, "/post/p8"),
            vec![content::ContentElement::paragraph_from_markdown(
                "See [A](/author/a) for more."
            )]
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3", "/post/p8"])
        );
    }

    /// Body references do not count yet (ADR-059).
    #[test]
    fn body_links_do_not_count() {
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p9.md",
            "# Post nine\n\n2025-06-04\n\nnotes\n\n[b](/author/b)\n\n----\n\nSee [A](/author/a).\n",
        );
        let conductor = make_conductor(&tmp);
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/b", "posts"),
            urls(&["/post/p2", "/post/p9"])
        );
    }

    // ── Parity: DataGraph and store adapters read the same field text ───────

    #[test]
    fn datagraph_and_store_adapters_agree_on_field_text() {
        use expressions::ThreadItem;
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        for url in ["/post/p1", "/post/p2", "/post/p3"] {
            let root = conductor.document_by_url(url).unwrap();
            let sem = conductor.semantic_content_by_url(url).unwrap();
            let data = conductor.datagraph_for_document(root).unwrap();
            let store = conductor.node_store.read().unwrap();
            let item = node_store_bridge::store_pipeline::SemItem {
                store: &store,
                sem_root: sem,
                url: url.to_string(),
            };
            for field in ["title", "published", "category"] {
                let from_graph = expressions::graph_field_text(&data, field);
                assert!(from_graph.is_some(), "{url} {field}");
                assert_eq!(item.field_text(field), from_graph, "{url} {field}");
            }
        }
    }

    /// A heading stores its source; thread operations read its plain text on
    /// both adapters (ADR-063).
    #[test]
    fn store_field_text_heading_is_plain_and_matches_graph() {
        use expressions::ThreadItem;
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p7.md",
            "# *Post* \\<seven> <!-- c -->\n\n2026-04-10\n\nnotes\n\n[a](/author/a)\n\n----\n\nBody.\n",
        );
        let conductor = make_conductor(&tmp);
        let url = "/post/p7";
        let root = conductor.document_by_url(url).unwrap();
        let sem = conductor.semantic_content_by_url(url).unwrap();
        let data = conductor.datagraph_for_document(root).unwrap();
        let store = conductor.node_store.read().unwrap();
        let item = node_store_bridge::store_pipeline::SemItem {
            store: &store,
            sem_root: sem,
            url: url.to_string(),
        };
        assert_eq!(item.field_text("title").as_deref(), Some("Post <seven>"));
        assert_eq!(
            item.field_text("title"),
            expressions::graph_field_text(&data, "title")
        );
    }

    // ── Derived state ────────────────────────────────────────────────────────

    #[test]
    fn full_build_records_query_nodes_and_fingerprints() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let snapshot = conductor.snapshot_query_results();
        let key = |p: &str, s: &str| (p.to_string(), s.to_string());
        assert_eq!(
            snapshot.get(&key("/page/home", "latest")),
            Some(&urls(&["/post/p3", "/post/p2"]))
        );
        assert_eq!(
            snapshot.get(&key("/page/home", "news")),
            Some(&urls(&["/post/p2", "/post/p3"]))
        );
        assert_eq!(
            snapshot.get(&key("/author/a", "posts")),
            Some(&urls(&["/post/p1", "/post/p3"]))
        );
        assert_eq!(
            snapshot.get(&key("/author/b", "posts")),
            Some(&urls(&["/post/p2"]))
        );
        assert_eq!(snapshot.len(), 4);
        let store = conductor.node_store.read().unwrap();
        for url in ["/post/p1", "/author/a", "/page/home"] {
            let root = conductor.document_by_url(url).unwrap();
            assert_eq!(
                node_store_bridge::store_pipeline::fingerprint_of(&store, root),
                Some(node_store_bridge::store_pipeline::preamble_fingerprint(
                    &store, root
                )),
                "{url}"
            );
        }
    }

    /// Re-ingesting a page with queries replaces its query nodes: one per
    /// thread expression, recorded against the live semantic root.
    #[test]
    fn reingested_page_keeps_one_query_node_per_expression() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        for _ in 0..2 {
            assert_eq!(
                document_changed(&conductor, &tmp, "content/page/home.md", HOME),
                set(&["/page/home"])
            );
        }
        let snapshot = conductor.snapshot_query_results();
        assert_eq!(snapshot.len(), 4);
        let store = conductor.node_store.read().unwrap();
        let stem_node = conductor.stem_nodes.read().unwrap()["post"];
        let queries = node_store_bridge::store_pipeline::queries_over(&store, stem_node);
        assert_eq!(queries.len(), 4, "{queries:?}");
        drop(store);
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3", "/post/p2"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "news"),
            urls(&["/post/p2", "/post/p3"])
        );
    }

    // ── Incremental propagation (step 9) ─────────────────────────────────────

    /// 1: a body-only edit re-renders only that document.
    #[test]
    fn incremental_body_only_edit_renders_only_the_document() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = post("Post one", "a", "2026-01-10", "notes")
            .replace("Body of Post one.", "A new body.");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/post/p1.md", &text),
            set(&["/post/p1"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3", "/post/p2"])
        );
    }

    /// 2: a sort key change re-renders the page whose result changes and the
    /// page whose result contains the document.
    #[test]
    fn incremental_published_change_via_document_changed() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = post("Post one", "a", "2026-04-01", "notes");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/post/p1.md", &text),
            set(&["/post/p1", "/page/home", "/author/a"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p1", "/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3"])
        );
    }

    /// 3: the same edit through a block edit.
    #[test]
    fn incremental_published_change_via_block_edit() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let result = conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: editorial_types::ContentPath::parse("content/post/p1.md").expect("valid"),
            selection: r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/p1.md") "published")) "paragraph" 0)"#
                .to_string(),
            base: editorial_types::BlockText::Markdown("2026-01-10".to_string()),
            proposed: editorial_types::BlockText::Markdown("2026-04-01".to_string()),
            intent: editorial_types::BlockEditIntent::Apply,
            reason: None,
        }));
        assert!(
            matches!(
                result.response,
                Response::BlockEdited(editorial_types::BlockEditResponse::Done(
                    editorial_types::BlockEditOutcome::Applied { .. }
                ))
            ),
            "{:?}",
            result.response
        );
        assert_eq!(
            pages_of(&result),
            set(&["/post/p1", "/page/home", "/author/a"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p1", "/post/p3"])
        );
    }

    /// 4: a title change re-renders every page whose result contains it.
    #[test]
    fn incremental_title_change() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = post("Post three renamed", "a", "2026-03-10", "news");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/post/p3.md", &text),
            set(&["/post/p3", "/page/home", "/author/a"])
        );
    }

    /// 5: changing the author moves the post between author pages.
    #[test]
    fn incremental_author_change() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = post("Post one", "b", "2026-01-10", "notes");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/post/p1.md", &text),
            set(&["/post/p1", "/author/a", "/author/b"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/b", "posts"),
            urls(&["/post/p1", "/post/p2"])
        );
    }

    /// 6a/6b: create a post (scaffold), then fill it in.
    #[test]
    fn incremental_create_then_fill() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);

        // 6a: the empty scaffold has no `published`, which sorts first under
        // `:desc`, so `latest` changes.
        let result = conductor.handle_command(Command::CreateContent {
            stem: "post".to_string(),
            slug: "p4".to_string(),
        });
        assert!(
            matches!(result.response, Response::ContentCreated(_)),
            "{:?}",
            result.response
        );
        assert_eq!(pages_of(&result), set(&["/post/p4", "/page/home"]));
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p4", "/post/p3"])
        );

        // 6b
        let text = post("Post four", "b", "2026-05-01", "notes");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/post/p4.md", &text),
            set(&["/post/p4", "/page/home", "/author/b"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p4", "/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/b", "posts"),
            urls(&["/post/p2", "/post/p4"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3"])
        );
    }

    /// 7: removing a post is a full rebuild (D9); the results reflect it.
    #[test]
    fn file_removed_full_rebuild() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let p2 = tmp.path().join("content/post/p2.md");
        std::fs::remove_file(&p2).unwrap();
        conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse("content/post/p2.md").expect("valid")],
        });
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3", "/post/p1"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "news"),
            urls(&["/post/p3"])
        );
        assert!(slot_urls(&conductor, "/author/b", "posts").is_empty());
    }

    /// B1: external URLs, anchors and `:path` text in running prose are not
    /// page references.
    #[test]
    fn external_links_are_not_page_references() {
        let tmp = build_site();
        write(
            &tmp,
            "content/author/c.md",
            "# Author C\n\n[Site](https://example.com)\n\n----\n\nAbout C.\n",
        );
        write(
            &tmp,
            "content/post/p10.md",
            &post_with(
                "Post ten",
                "2025-06-05",
                "notes",
                "See [x](https://x.org) and [a](:author/a).",
            ),
        );
        let conductor = make_conductor(&tmp);
        assert!(conductor.query_edges_from("/author/c").is_empty());
        let targets: Vec<String> = conductor
            .query_edges_from("/post/p10")
            .into_iter()
            .map(|e| e.target.as_str().to_string())
            .collect();
        assert_eq!(targets, urls(&["/author/a"]));
    }

    /// B2: a body-only edit of a page with queries renders only that page
    /// and keeps its results and query nodes.
    #[test]
    fn body_only_edit_of_a_page_with_queries() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = AUTHOR_A.replace("About A.", "All about A.");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/author/a.md", &text),
            set(&["/author/a"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p1", "/post/p3"])
        );
        let store = conductor.node_store.read().unwrap();
        let post_stem = conductor.stem_nodes.read().unwrap()["post"];
        assert_eq!(
            node_store_bridge::store_pipeline::queries_over(&store, post_stem).len(),
            4
        );
    }

    fn queries_over_stem(conductor: &Conductor, stem: &str) -> usize {
        let store = conductor.node_store.read().unwrap();
        let node = conductor.stem_nodes.read().unwrap()[stem];
        node_store_bridge::store_pipeline::queries_over(&store, node).len()
    }

    /// B4: removing a thread expression removes its query node.
    #[test]
    fn removing_a_thread_removes_its_query() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = HOME.replace(
            "[In the news](->> :post (filter :category \"news\")) {#news}\n\n",
            "",
        );
        assert_eq!(
            document_changed(&conductor, &tmp, "content/page/home.md", &text),
            set(&["/page/home"])
        );
        let snapshot = conductor.snapshot_query_results();
        assert_eq!(snapshot.len(), 3, "{snapshot:?}");
        assert!(!snapshot.contains_key(&("/page/home".to_string(), "news".to_string())));
        assert_eq!(queries_over_stem(&conductor, "post"), 3);
    }

    /// B4: changing a thread's source moves its query to the new stem node.
    #[test]
    fn changing_a_thread_source_moves_its_query() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        let text = HOME.replace(
            "[In the news](->> :post (filter :category \"news\")) {#news}",
            "[In the news](->> :author (sort-by :name)) {#news}",
        );
        document_changed(&conductor, &tmp, "content/page/home.md", &text);
        assert_eq!(queries_over_stem(&conductor, "post"), 3);
        assert_eq!(queries_over_stem(&conductor, "author"), 1);
        assert_eq!(
            slot_urls(&conductor, "/page/home", "news"),
            urls(&["/author/a", "/author/b"])
        );
    }

    /// Replace the text of the first Text node under `slot`'s first element of
    /// `url` (or its `attr` attribute), in place.
    fn edit_in_place(
        conductor: &Conductor,
        url: &str,
        slot: &str,
        attr: Option<&str>,
        value: &str,
    ) {
        let root = conductor.document_by_url(url).unwrap();
        let mut store = conductor.node_store.write().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                .unwrap();
        let slot_node = store
            .children(preamble)
            .into_iter()
            .find(|&s| {
                node_store_bridge::content_bridge::find_attr_text(&store, s, "name").as_deref()
                    == Some(slot)
            })
            .unwrap();
        let block = store.children(slot_node)[0];
        let text = match attr {
            // A lone link is a paragraph: its `href` is on the `inline-link`
            // it holds (ADR-059 §12).
            Some(attr) => {
                let holder = store
                    .children(block)
                    .into_iter()
                    .find(|c| matches!(store.get(*c), Some(node_store::Node::Element(_))))
                    .unwrap_or(block);
                store
                    .attributes(holder)
                    .into_iter()
                    .find(|(n, _)| store.resolve_name(*n) == attr)
                    .map(|(_, v)| v)
                    .unwrap()
            }
            None => store.children(block)[0],
        };
        store.replace_node(text, node_store::Node::Text(value.to_string()));
    }

    /// B4: a batch of in-place edits renders the exact union of affected pages.
    #[test]
    fn modified_nodes_batch_renders_the_union() {
        let tmp = build_site();
        let conductor = make_conductor(&tmp);
        edit_in_place(&conductor, "/post/p1", "author", Some("href"), "/author/b");
        edit_in_place(&conductor, "/post/p3", "published", None, "2026-04-01");
        let (rebuilt, failed) = conductor.rebuild_pages_for_modified_nodes(&[
            PathBuf::from("content/post/p1.md"),
            PathBuf::from("content/post/p3.md"),
        ]);
        assert!(failed.is_empty(), "{failed:?}");
        let rebuilt: BTreeSet<String> = rebuilt.into_iter().collect();
        assert_eq!(
            rebuilt,
            set(&[
                "/post/p1",
                "/post/p3",
                "/page/home",
                "/author/a",
                "/author/b"
            ])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/a", "posts"),
            urls(&["/post/p3"])
        );
        assert_eq!(
            slot_urls(&conductor, "/author/b", "posts"),
            urls(&["/post/p1", "/post/p2"])
        );
        assert_eq!(
            slot_urls(&conductor, "/page/home", "latest"),
            urls(&["/post/p3", "/post/p2"])
        );
    }

    /// 8: fixing U3's p7 (moving its link into the author slot) keeps it in
    /// A's posts exactly once and re-renders p7 and A (D3a: p7's preamble
    /// changed and p7 is in A's result).
    #[test]
    fn incremental_fixing_an_unassigned_reference() {
        let tmp = build_site();
        write(
            &tmp,
            "content/post/p7.md",
            &post_with(
                "Post seven",
                "2025-12-01",
                "notes",
                "[A](/author/a) {#nope}",
            ),
        );
        let conductor = make_conductor(&tmp);
        assert_eq!(
            count(&slot_urls(&conductor, "/author/a", "posts"), "/post/p7"),
            1
        );
        let fixed = post_with("Post seven", "2025-12-01", "notes", "[A](/author/a)");
        assert_eq!(
            document_changed(&conductor, &tmp, "content/post/p7.md", &fixed),
            set(&["/post/p7", "/author/a"])
        );
        let posts = slot_urls(&conductor, "/author/a", "posts");
        assert_eq!(count(&posts, "/post/p7"), 1, "{posts:?}");
        assert_eq!(posts, urls(&["/post/p1", "/post/p3", "/post/p7"]));
    }
}

/// ADR-059 stages 4 and 5: pending suggestions made before headings and
/// paragraphs held inline nodes are discarded once per step, the last step
/// applied being recorded in `.presemble/suggestions/ned/MODEL`.
#[cfg(test)]
mod model_migration_tests {
    use super::*;
    use editorial_types::{
        Author, ContentPath, NedMutation, NedSuggestion, NedSuggestionStatus, SuggestionId,
    };

    const FILE: &str = "content/post/first.md";
    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert></body></html>"#;
    const CONTENT: &str =
        "# First Post\n\n----\n\nAlpha para.\n\nBeta para.\n\n```\nlet x = 1;\n```\n";
    const TITLE_TEXT: &str = r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "title")))"#;
    const PARAGRAPH_TEXT: &str = r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "body")) "paragraph" 0))"#;
    const BODY_PARAGRAPH: &str = r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "body")) "paragraph" 0)"#;
    const CODE_BLOCK: &str = r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "body")) "code-block" 0)"#;
    const CODE_TEXT: &str = r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "body")) "code-block" 0))"#;

    fn site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), CONTENT).unwrap();
        tmp
    }

    fn dir(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join(".presemble/suggestions/ned")
    }

    /// Write a suggestion record as an older conductor would have.
    fn write_suggestion(
        tmp: &tempfile::TempDir,
        id: &str,
        selection: &str,
        mutation: NedMutation,
        status: NedSuggestionStatus,
    ) {
        let suggestion = NedSuggestion {
            id: SuggestionId::from(id.to_string()),
            author: Author::Claude,
            file: ContentPath::parse(FILE).expect("valid"),
            selection: selection.to_string(),
            mutation,
            workspace_hash: "untracked".to_string(),
            reason: "test".to_string(),
            status,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            preview: None,
        };
        std::fs::create_dir_all(dir(tmp)).unwrap();
        std::fs::write(
            dir(tmp).join(format!("{id}.json")),
            serde_json::to_string_pretty(&suggestion).unwrap(),
        )
        .unwrap();
    }

    fn start(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    fn listed(conductor: &Conductor) -> Vec<String> {
        let mut ids: Vec<String> = conductor
            .ned_suggestions
            .read()
            .unwrap()
            .keys()
            .map(|id| id.to_string())
            .collect();
        ids.sort();
        ids
    }

    fn file_exists(tmp: &tempfile::TempDir, id: &str) -> bool {
        dir(tmp).join(format!("{id}.json")).exists()
    }

    fn marker(tmp: &tempfile::TempDir) -> Option<String> {
        std::fs::read_to_string(dir(tmp).join("MODEL")).ok()
    }

    #[test]
    fn pending_heading_suggestion_from_before_stage_4_is_discarded_on_load() {
        let tmp = site();
        write_suggestion(
            &tmp,
            "sug-heading",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);

        assert!(
            !listed(&conductor).contains(&"sug-heading".to_string()),
            "not listed"
        );
        assert!(!file_exists(&tmp, "sug-heading"), "its file is gone");
        assert_eq!(
            marker(&tmp).as_deref(),
            Some("list-containers"),
            "the last step is recorded"
        );
    }

    #[test]
    fn pending_heading_payload_is_discarded() {
        let tmp = site();
        let heading = ned::NodeTree::element("heading")
            .with_attr("level", "2")
            .with_child(ned::NodeTree::text("New section"));
        write_suggestion(
            &tmp,
            "sug-payload",
            BODY_PARAGRAPH,
            NedMutation::InsertAfter(vec![heading]),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);

        assert!(listed(&conductor).is_empty(), "{:?}", listed(&conductor));
        assert!(!file_exists(&tmp, "sug-payload"));
    }

    #[test]
    fn pending_suggestion_on_another_kind_survives_the_upgrade() {
        let tmp = site();
        write_suggestion(
            &tmp,
            "sug-code",
            CODE_TEXT,
            NedMutation::SetText("let x = 2;".to_string()),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);

        assert_eq!(listed(&conductor), vec!["sug-code".to_string()]);
        assert!(file_exists(&tmp, "sug-code"));
        assert_eq!(
            marker(&tmp).as_deref(),
            Some("list-containers"),
            "marked all the same"
        );
    }

    /// The user's decision: every pending suggestion that touches a paragraph
    /// is discarded, plain text or not.
    #[test]
    fn pending_paragraph_suggestion_is_discarded() {
        let tmp = site();
        write_suggestion(
            &tmp,
            "sug-text",
            PARAGRAPH_TEXT,
            NedMutation::SetText("Alpha paragraph.".to_string()),
            NedSuggestionStatus::Pending,
        );
        write_suggestion(
            &tmp,
            "sug-search",
            PARAGRAPH_TEXT,
            NedMutation::SearchReplace {
                search: "Alpha".to_string(),
                replace: "*Alpha*".to_string(),
            },
            NedSuggestionStatus::Pending,
        );
        write_suggestion(
            &tmp,
            "sug-block",
            BODY_PARAGRAPH,
            NedMutation::Delete,
            NedSuggestionStatus::Pending,
        );
        let paragraph = ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text("New."));
        write_suggestion(
            &tmp,
            "sug-payload",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![paragraph]),
            NedSuggestionStatus::Pending,
        );
        write_suggestion(
            &tmp,
            "sug-code",
            CODE_TEXT,
            NedMutation::SetText("let x = 2;".to_string()),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);

        assert_eq!(
            listed(&conductor),
            vec!["sug-code".to_string()],
            "only the code suggestion is left"
        );
        for id in ["sug-text", "sug-search", "sug-block", "sug-payload"] {
            assert!(!file_exists(&tmp, id), "{id}: its file is gone");
        }
        assert!(file_exists(&tmp, "sug-code"));
    }

    /// An absent `MODEL` runs every step: `heading-inlines`,
    /// `paragraph-inlines`, `link-image-paragraphs`, `table-cells`,
    /// `blockquote-containers` and `list-containers`. Each recorded step runs
    /// every step after it, and none before it: `table-cells` runs
    /// `blockquote-containers` and `list-containers`, `blockquote-containers`
    /// runs only `list-containers`, and `list-containers` runs none. Each step
    /// runs once: a suggestion made after the migration survives the next
    /// start.
    #[test]
    fn migration_runs_each_step_once_in_order() {
        let both = |tmp: &tempfile::TempDir| {
            write_suggestion(
                tmp,
                "sug-heading",
                TITLE_TEXT,
                NedMutation::SetText("Renamed".to_string()),
                NedSuggestionStatus::Pending,
            );
            write_suggestion(
                tmp,
                "sug-para",
                PARAGRAPH_TEXT,
                NedMutation::SetText("Alpha paragraph.".to_string()),
                NedSuggestionStatus::Pending,
            );
        };

        // No MODEL: the heading and paragraph steps run.
        let tmp = site();
        both(&tmp);
        assert_eq!(listed(&start(&tmp)), Vec::<String>::new());
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // heading-inlines: the paragraph step only; the heading suggestion
        // was made after headings held nodes.
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "heading-inlines").unwrap();
        both(&tmp);
        assert_eq!(listed(&start(&tmp)), vec!["sug-heading".to_string()]);
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // paragraph-inlines: only the link and image step runs; neither
        // suggestion holds a link or an image.
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "paragraph-inlines").unwrap();
        both(&tmp);
        assert_eq!(
            listed(&start(&tmp)),
            vec!["sug-heading".to_string(), "sug-para".to_string()]
        );
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // link-image-paragraphs: only the table step runs; neither
        // suggestion holds a table.
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "link-image-paragraphs").unwrap();
        both(&tmp);
        assert_eq!(
            listed(&start(&tmp)),
            vec!["sug-heading".to_string(), "sug-para".to_string()]
        );
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // table-cells: the blockquote and list steps run; neither suggestion
        // holds a blockquote or a list.
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "table-cells").unwrap();
        both(&tmp);
        assert_eq!(
            listed(&start(&tmp)),
            vec!["sug-heading".to_string(), "sug-para".to_string()]
        );
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // blockquote-containers: only the list step runs; neither
        // suggestion holds a list.
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "blockquote-containers").unwrap();
        both(&tmp);
        assert_eq!(
            listed(&start(&tmp)),
            vec!["sug-heading".to_string(), "sug-para".to_string()]
        );
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // list-containers: nothing runs.
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "list-containers").unwrap();
        both(&tmp);
        assert_eq!(
            listed(&start(&tmp)),
            vec!["sug-heading".to_string(), "sug-para".to_string()]
        );
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));

        // Once: a suggestion made after the migration survives the next start.
        let tmp = site();
        both(&tmp);
        drop(start(&tmp));
        both(&tmp);
        assert_eq!(
            listed(&start(&tmp)),
            vec!["sug-heading".to_string(), "sug-para".to_string()]
        );
    }

    /// The order of the steps, and what is left to run after each.
    #[test]
    fn model_steps_follow_the_recorded_one() {
        let names = |recorded: Option<&str>| -> Vec<&'static str> {
            model_steps_after(recorded)
                .iter()
                .map(|step| step.name)
                .collect()
        };
        assert_eq!(
            names(None),
            vec![
                "heading-inlines",
                "paragraph-inlines",
                "link-image-paragraphs",
                "table-cells",
                "blockquote-containers",
                "list-containers"
            ]
        );
        assert_eq!(
            names(Some("heading-inlines")),
            vec![
                "paragraph-inlines",
                "link-image-paragraphs",
                "table-cells",
                "blockquote-containers",
                "list-containers"
            ]
        );
        assert_eq!(
            names(Some("paragraph-inlines")),
            vec![
                "link-image-paragraphs",
                "table-cells",
                "blockquote-containers",
                "list-containers"
            ]
        );
        assert_eq!(
            names(Some("link-image-paragraphs")),
            vec!["table-cells", "blockquote-containers", "list-containers"]
        );
        assert_eq!(
            names(Some("table-cells")),
            vec!["blockquote-containers", "list-containers"]
        );
        assert_eq!(
            names(Some("blockquote-containers")),
            vec!["list-containers"]
        );
        assert!(names(Some("list-containers")).is_empty());
        // A name this conductor does not know was written by a newer one:
        // nothing runs.
        assert!(names(Some("something-newer")).is_empty());
    }

    /// A pending suggestion whose payload holds a `link` or `image` block is
    /// discarded once (the kinds are gone, ADR-059 §12); a paragraph that
    /// holds an inline link, other pending suggestions and history stay.
    #[test]
    fn pending_link_and_image_payloads_are_discarded() {
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "paragraph-inlines").unwrap();
        let link = ned::NodeTree::element("link")
            .with_attr("href", "/x")
            .with_child(ned::NodeTree::text("t"));
        let image = ned::NodeTree::element("image").with_attr("path", "p.jpg");
        let nested = ned::NodeTree::element("blockquote").with_child(link.clone());
        let inline_link_paragraph = ned::NodeTree::element("paragraph").with_child(
            ned::NodeTree::element("inline-link")
                .with_attr("href", "/x")
                .with_child(ned::NodeTree::text("t")),
        );
        let pending = NedSuggestionStatus::Pending;
        write_suggestion(
            &tmp,
            "sug-link",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![link.clone()]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-image",
            CODE_BLOCK,
            NedMutation::Replace(vec![image]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-nested",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![nested]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-inline-link",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![inline_link_paragraph]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-title",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            pending,
        );
        write_suggestion(
            &tmp,
            "sug-accepted-link",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![link]),
            NedSuggestionStatus::Accepted,
        );

        let conductor = start(&tmp);

        assert_eq!(
            listed(&conductor),
            vec![
                "sug-accepted-link".to_string(),
                "sug-inline-link".to_string(),
                "sug-title".to_string()
            ]
        );
        for id in ["sug-link", "sug-image", "sug-nested"] {
            assert!(!file_exists(&tmp, id), "{id}: its file is gone");
        }
        assert!(file_exists(&tmp, "sug-inline-link") && file_exists(&tmp, "sug-accepted-link"));
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));
    }

    /// Table cells became inline containers (ADR-059 stage 6): a pending
    /// suggestion whose payload holds a table, in the old shape or not, is
    /// discarded once. Other payloads, other pending suggestions and history
    /// stay.
    #[test]
    fn pending_table_payloads_are_discarded() {
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "link-image-paragraphs").unwrap();
        let old_table = ned::NodeTree::element("table")
            .with_child(ned::NodeTree::element("headers").with_child(ned::NodeTree::text("h")))
            .with_child(
                ned::NodeTree::element("rows")
                    .with_child(ned::NodeTree::element("row").with_child(ned::NodeTree::text("a"))),
            );
        let new_table =
            ned::NodeTree::element("table")
                .with_child(ned::NodeTree::element("head").with_child(
                    ned::NodeTree::element("cell").with_child(ned::NodeTree::text("h")),
                ))
                .with_child(ned::NodeTree::element("rows"));
        let nested = ned::NodeTree::element("blockquote").with_child(old_table.clone());
        let pending = NedSuggestionStatus::Pending;
        write_suggestion(
            &tmp,
            "sug-old",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![old_table.clone()]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-new",
            CODE_BLOCK,
            NedMutation::Replace(vec![new_table]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-nested",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![nested]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-title",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            pending,
        );
        write_suggestion(
            &tmp,
            "sug-accepted-table",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![old_table]),
            NedSuggestionStatus::Accepted,
        );

        let conductor = start(&tmp);

        assert_eq!(
            listed(&conductor),
            vec!["sug-accepted-table".to_string(), "sug-title".to_string()]
        );
        for id in ["sug-old", "sug-new", "sug-nested"] {
            assert!(!file_exists(&tmp, id), "{id}: its file is gone");
        }
        assert!(file_exists(&tmp, "sug-title") && file_exists(&tmp, "sug-accepted-table"));
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));
    }

    /// Blockquotes became containers of blocks (ADR-059 stage 8): a pending
    /// suggestion whose payload holds a blockquote, in the old shape (one text
    /// child) or the new one, is discarded once. Other payloads, other pending
    /// suggestions and history stay.
    #[test]
    fn pending_blockquote_payloads_are_discarded() {
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "table-cells").unwrap();
        let old_quote = ned::NodeTree::element("blockquote").with_child(ned::NodeTree::text("q"));
        let new_quote = ned::NodeTree::element("blockquote")
            .with_child(ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text("q")));
        let nested = ned::NodeTree::element("blockquote").with_child(new_quote.clone());
        let paragraph = ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text("p"));
        let pending = NedSuggestionStatus::Pending;
        write_suggestion(
            &tmp,
            "sug-old",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![old_quote.clone()]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-new",
            CODE_BLOCK,
            NedMutation::Replace(vec![new_quote]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-nested",
            CODE_BLOCK,
            NedMutation::InsertChild(vec![nested]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-paragraph",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![paragraph]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-title",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            pending,
        );
        write_suggestion(
            &tmp,
            "sug-accepted-quote",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![old_quote]),
            NedSuggestionStatus::Accepted,
        );

        let conductor = start(&tmp);

        assert_eq!(
            listed(&conductor),
            vec![
                "sug-accepted-quote".to_string(),
                "sug-paragraph".to_string(),
                "sug-title".to_string()
            ]
        );
        for id in ["sug-old", "sug-new", "sug-nested"] {
            assert!(!file_exists(&tmp, id), "{id}: its file is gone");
        }
        assert!(file_exists(&tmp, "sug-title") && file_exists(&tmp, "sug-accepted-quote"));
        assert!(file_exists(&tmp, "sug-paragraph"));
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));
    }

    /// Guard: a text suggestion on a blockquote made before stage 8 (its
    /// selection is `ned/text-in` of the quote) is never misapplied. It is
    /// accepted after the migration has run, and is refused or goes Stale with
    /// the file unchanged.
    #[test]
    fn text_suggestion_on_a_blockquote_from_before_stage_8_is_stale() {
        let tmp = site();
        let content = "# First Post\n\n----\n\n> a\n>\n> b\n";
        std::fs::write(tmp.path().join(FILE), content).unwrap();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "list-containers").unwrap();
        let quote_text = r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "body")) "blockquote" 0))"#;
        write_suggestion(
            &tmp,
            "sug-quote-text",
            quote_text,
            NedMutation::SetText("rewritten".to_string()),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);
        assert_eq!(
            listed(&conductor),
            vec!["sug-quote-text".to_string()],
            "the migration is done"
        );
        let result = conductor.handle_command(Command::AcceptNedSuggestion {
            id: SuggestionId::from("sug-quote-text".to_string()),
        });

        assert!(
            !matches!(result.response, Response::Applied { .. }),
            "not applied: {:?}",
            result.response
        );
        let map = conductor.ned_suggestions.read().unwrap();
        let status = &map[&SuggestionId::from("sug-quote-text".to_string())].status;
        assert!(
            matches!(
                status,
                NedSuggestionStatus::Stale { .. } | NedSuggestionStatus::Pending
            ),
            "{status:?}"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(FILE)).unwrap(),
            content
        );
        assert!(
            conductor.dirty_docs.read().unwrap().is_empty(),
            "nothing was changed"
        );
    }

    /// Lists became containers of items of blocks (ADR-059 stage 9): a pending
    /// suggestion whose payload holds a list, in the old shape (one text child)
    /// or the new one, is discarded once. Other payloads, other pending
    /// suggestions and history stay.
    #[test]
    fn pending_list_payloads_are_discarded() {
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "blockquote-containers").unwrap();
        let old_list = ned::NodeTree::element("list")
            .with_attr("ordered", "false")
            .with_child(ned::NodeTree::text("- a\n- b"));
        let new_list = ned::NodeTree::element("list")
            .with_attr("ordered", "false")
            .with_attr("tight", "true")
            .with_child(ned::NodeTree::element("item").with_child(
                ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text("a")),
            ));
        let nested = ned::NodeTree::element("blockquote").with_child(new_list.clone());
        let paragraph = ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text("p"));
        let pending = NedSuggestionStatus::Pending;
        write_suggestion(
            &tmp,
            "sug-old",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![old_list.clone()]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-new",
            CODE_BLOCK,
            NedMutation::Replace(vec![new_list]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-nested",
            CODE_BLOCK,
            NedMutation::InsertChild(vec![nested]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-paragraph",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![paragraph]),
            pending.clone(),
        );
        write_suggestion(
            &tmp,
            "sug-title",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            pending,
        );
        write_suggestion(
            &tmp,
            "sug-accepted-list",
            CODE_BLOCK,
            NedMutation::InsertAfter(vec![old_list]),
            NedSuggestionStatus::Accepted,
        );

        let conductor = start(&tmp);

        assert_eq!(
            listed(&conductor),
            vec![
                "sug-accepted-list".to_string(),
                "sug-paragraph".to_string(),
                "sug-title".to_string()
            ]
        );
        for id in ["sug-old", "sug-new", "sug-nested"] {
            assert!(!file_exists(&tmp, id), "{id}: its file is gone");
        }
        assert!(file_exists(&tmp, "sug-title") && file_exists(&tmp, "sug-accepted-list"));
        assert!(file_exists(&tmp, "sug-paragraph"));
        assert_eq!(marker(&tmp).as_deref(), Some("list-containers"));
    }

    /// Guard: a text suggestion on a list made before stage 9 (its selection is
    /// `ned/text-in` of the list, which then held one text) is never
    /// misapplied. It is accepted after the migration has run, and is refused
    /// or goes Stale with the file unchanged.
    #[test]
    fn text_suggestion_on_a_list_from_before_stage_9_is_stale() {
        let tmp = site();
        let content = "# First Post\n\n----\n\n- a\n- b\n";
        std::fs::write(tmp.path().join(FILE), content).unwrap();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "list-containers").unwrap();
        let list_text = r#"(ned/text-in (ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/first.md") "body")) "list" 0))"#;
        write_suggestion(
            &tmp,
            "sug-list-text",
            list_text,
            NedMutation::SetText("rewritten".to_string()),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);
        assert_eq!(
            listed(&conductor),
            vec!["sug-list-text".to_string()],
            "the migration is done"
        );
        let result = conductor.handle_command(Command::AcceptNedSuggestion {
            id: SuggestionId::from("sug-list-text".to_string()),
        });

        assert!(
            !matches!(result.response, Response::Applied { .. }),
            "not applied: {:?}",
            result.response
        );
        let map = conductor.ned_suggestions.read().unwrap();
        let status = &map[&SuggestionId::from("sug-list-text".to_string())].status;
        assert!(
            matches!(
                status,
                NedSuggestionStatus::Stale { .. } | NedSuggestionStatus::Pending
            ),
            "{status:?}"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(FILE)).unwrap(),
            content
        );
        assert!(
            conductor.dirty_docs.read().unwrap().is_empty(),
            "nothing was changed"
        );
    }

    #[test]
    fn history_is_not_discarded() {
        let tmp = site();
        write_suggestion(
            &tmp,
            "sug-accepted",
            TITLE_TEXT,
            NedMutation::SetText("A".to_string()),
            NedSuggestionStatus::Accepted,
        );
        write_suggestion(
            &tmp,
            "sug-rejected",
            TITLE_TEXT,
            NedMutation::SetText("B".to_string()),
            NedSuggestionStatus::Rejected,
        );
        write_suggestion(
            &tmp,
            "sug-accepted-para",
            PARAGRAPH_TEXT,
            NedMutation::SetText("C".to_string()),
            NedSuggestionStatus::Accepted,
        );

        let conductor = start(&tmp);

        assert_eq!(
            listed(&conductor),
            vec![
                "sug-accepted".to_string(),
                "sug-accepted-para".to_string(),
                "sug-rejected".to_string()
            ]
        );
        assert!(file_exists(&tmp, "sug-accepted") && file_exists(&tmp, "sug-rejected"));
        assert!(file_exists(&tmp, "sug-accepted-para"));
    }

    /// Every file under `.presemble` with its bytes, by path.
    fn presemble_snapshot(tmp: &tempfile::TempDir) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn walk(dir: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(&tmp.path().join(".presemble"), &mut out);
        out
    }

    /// `presemble build` loads the site to validate it: that never writes to
    /// `.presemble`, neither the legacy cleanup nor a model discard.
    #[test]
    fn loading_a_site_never_writes_to_presemble() {
        let tmp = site();
        write_suggestion(
            &tmp,
            "sug-heading",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            NedSuggestionStatus::Pending,
        );
        write_suggestion(
            &tmp,
            "sug-paragraph",
            PARAGRAPH_TEXT,
            NedMutation::SetText("Alpha paragraph.".to_string()),
            NedSuggestionStatus::Pending,
        );
        let legacy = tmp.path().join(".presemble/suggestions/sug-old.json");
        std::fs::write(&legacy, "{}").unwrap();
        let before = presemble_snapshot(&tmp);
        assert!(before.contains_key(&legacy));

        let _conductor = Conductor::load(tmp.path().to_path_buf()).expect("loads");

        assert_eq!(
            presemble_snapshot(&tmp),
            before,
            "the directory and every file are unchanged"
        );
    }

    #[test]
    fn suggestions_made_after_the_upgrade_are_never_discarded() {
        let tmp = site();
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("MODEL"), "paragraph-inlines").unwrap();
        write_suggestion(
            &tmp,
            "sug-new",
            TITLE_TEXT,
            NedMutation::SetText("Renamed".to_string()),
            NedSuggestionStatus::Pending,
        );

        let conductor = start(&tmp);

        assert_eq!(listed(&conductor), vec!["sug-new".to_string()]);
        assert!(file_exists(&tmp, "sug-new"));
    }
}

/// ADR-059 section 10, amended in stage 5b.3: every grammar slot is a
/// container in the store, an empty one renders an addressable placeholder in
/// editorial output, and a block edit on it fills it.
#[cfg(test)]
mod empty_slot_tests {
    use super::*;
    use editorial_types::{
        BlockEditCause, BlockEditIntent, BlockEditOutcome, BlockEditRequest, BlockEditResponse,
        BlockText, ContentPath,
    };

    const POST_SCHEMA: &str = "# Post title {#title}\noccurs\n: exactly once\n\n\
         Summary. {#summary}\noccurs\n: exactly once\n\n\
         [Author](/author/<name>) {#author}\noccurs\n: exactly once\n\n\
         ----\n\nBody.\nheadings\n: h3..h6\n";
    const AUTHOR_SCHEMA: &str = "# Author name {#name}\noccurs\n: exactly once\n";
    const POST_TEMPLATE: &str = r#"<html><body>
<presemble:insert data="(ref input.title)" as="h1"></presemble:insert>
<presemble:insert data="(ref input.summary)" as="p"></presemble:insert>
<presemble:insert data="(ref input.author)"></presemble:insert>
<div><presemble:insert data="(ref input.body)"></presemble:insert></div>
</body></html>"#;
    const AUTHOR_TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.name)" as="h1"></presemble:insert></body></html>"#;

    /// A site whose posts are: `empty` (an empty file, as a new file is),
    /// `partial` (a title and a body, no summary or author), `bare` (a separator
    /// and nothing else) and `full`.
    fn site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in [
            "schemas/post",
            "schemas/author",
            "templates/post",
            "templates/author",
            "content/post",
            "content/author",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), POST_SCHEMA).unwrap();
        std::fs::write(root.join("schemas/author/item.md"), AUTHOR_SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), POST_TEMPLATE).unwrap();
        std::fs::write(root.join("templates/author/item.html"), AUTHOR_TEMPLATE).unwrap();
        std::fs::write(root.join("content/author/jo.md"), "# Jo\n").unwrap();
        std::fs::write(root.join("content/post/empty.md"), "").unwrap();
        std::fs::write(
            root.join("content/post/partial.md"),
            "# Partial\n\n----\n\nBody para.\n",
        )
        .unwrap();
        std::fs::write(root.join("content/post/bare.md"), "\n----\n").unwrap();
        std::fs::write(
            root.join("content/post/full.md"),
            "# Full\n\nA summary.\n\n[Jo](/author/jo)\n\n----\n\nBody para.\n",
        )
        .unwrap();
        tmp
    }

    fn start(tmp: &tempfile::TempDir) -> Conductor {
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor")
    }

    fn slot_address(file: &str, slot: &str) -> String {
        format!(r#"(ned/slot (ned/doc-by-path "{file}") "{slot}")"#)
    }

    /// The names of the slot containers of the document at `file`, in order.
    fn slot_names(conductor: &Conductor, file: &str) -> Vec<String> {
        let root = conductor
            .doc_root_for_path(Path::new(file))
            .expect("document in the store");
        let store = conductor.node_store.read().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                .expect("preamble");
        store
            .children(preamble)
            .into_iter()
            .filter(|c| {
                matches!(store.get(*c), Some(node_store::Node::Element(n)) if store.resolve_name(*n) == "slot")
            })
            .filter_map(|c| node_store_bridge::content_bridge::find_attr_text(&store, c, "name"))
            .collect()
    }

    /// The blocks of the slot `slot` of the document at `file`, decoded.
    fn slot_blocks(conductor: &Conductor, file: &str, slot: &str) -> Vec<content::ContentElement> {
        let root = conductor
            .doc_root_for_path(Path::new(file))
            .expect("document in the store");
        let store = conductor.node_store.read().unwrap();
        let preamble =
            node_store_bridge::content_bridge::find_child_by_name(&store, root, "preamble")
                .expect("preamble");
        let slot_node = store
            .children(preamble)
            .into_iter()
            .find(|c| {
                node_store_bridge::content_bridge::find_attr_text(&store, *c, "name").as_deref()
                    == Some(slot)
            })
            .unwrap_or_else(|| panic!("no slot container {slot} in {file}"));
        store
            .children(slot_node)
            .into_iter()
            .map(|b| {
                node_store_bridge::content_bridge::block_from_node(&store, b).expect("a block")
            })
            .collect()
    }

    fn fill(file: &str, slot: &str, proposed: &str, intent: BlockEditIntent) -> Command {
        Command::BlockEdit(BlockEditRequest {
            file: ContentPath::parse(file).expect("valid"),
            selection: slot_address(file, slot),
            base: BlockText::Markdown(String::new()),
            proposed: BlockText::Markdown(proposed.to_string()),
            intent,
            reason: None,
        })
    }

    fn applied(result: &CommandResult) -> bool {
        matches!(
            &result.response,
            Response::BlockEdited(BlockEditResponse::Done(BlockEditOutcome::Applied { .. }))
        )
    }

    /// Every grammar slot of a document is a container in the store, empty or
    /// not, in grammar order: a new file, a file that ends at its separator,
    /// and a file with slots left out.
    #[test]
    fn a_new_file_has_every_grammar_slot_as_a_container() {
        let tmp = site();
        let conductor = start(&tmp);
        for file in [
            "content/post/empty.md",
            "content/post/bare.md",
            "content/post/partial.md",
            "content/post/full.md",
        ] {
            assert_eq!(
                slot_names(&conductor, file),
                ["title", "summary", "author"],
                "{file}"
            );
        }
    }

    /// An empty slot renders a placeholder that names the slot container as its
    /// address and has empty markdown, in editorial output.
    #[test]
    fn empty_slot_placeholder_carries_its_address() {
        let tmp = site();
        let conductor = start(&tmp);
        let html = conductor
            .render_page("/post/partial", &RenderMode::View)
            .expect("renders");
        let placeholders: Vec<&str> = html
            .split('<')
            .filter(|chunk| chunk.contains("presemble-suggestion"))
            .collect();
        for slot in ["summary", "author"] {
            let tag = placeholders
                .iter()
                .find(|t| t.contains(&format!(r#"data-presemble-slot="{slot}""#)))
                .unwrap_or_else(|| panic!("no placeholder for {slot}: {html}"));
            let address = slot_address("content/post/partial.md", slot).replace('"', "&quot;");
            assert!(
                tag.contains(&format!(r#"data-presemble-address="{address}""#)),
                "{slot}: {tag}"
            );
            assert!(tag.contains(r#"data-presemble-md="""#), "{slot}: {tag}");
        }
    }

    /// A block edit whose selection is an empty slot container and whose base is
    /// empty fills the slot: a heading slot, a paragraph slot and a link slot.
    #[test]
    fn block_edit_fills_an_empty_slot() {
        let tmp = site();
        let conductor = start(&tmp);
        let file = "content/post/empty.md";

        let result = conductor.handle_command(fill(
            file,
            "title",
            "My *first* post",
            BlockEditIntent::Apply,
        ));
        assert!(applied(&result), "title: {:?}", result.response);
        let title = slot_blocks(&conductor, file, "title");
        assert!(
            matches!(title.as_slice(), [content::ContentElement::Heading { level, .. }] if level.value() == 1),
            "{title:?}"
        );

        let result =
            conductor.handle_command(fill(file, "summary", "A summary.", BlockEditIntent::Apply));
        assert!(applied(&result), "summary: {:?}", result.response);
        assert_eq!(
            slot_blocks(&conductor, file, "summary"),
            [content::ContentElement::paragraph_from_markdown(
                "A summary."
            )]
        );

        let result = conductor.handle_command(fill(
            file,
            "author",
            "[Jo](/author/jo)",
            BlockEditIntent::Apply,
        ));
        assert!(applied(&result), "author: {:?}", result.response);
        let author = slot_blocks(&conductor, file, "author");
        assert!(
            matches!(author.as_slice(), [el] if el.sole_link().is_some_and(|l| l.destination == "/author/jo")),
            "{author:?}"
        );
        assert!(
            conductor
                .dirty_docs()
                .read()
                .unwrap()
                .contains(Path::new(file))
        );
    }

    /// Suggest shares the lowering: the suggestion fills the slot when accepted.
    #[test]
    fn block_edit_suggested_into_an_empty_slot_fills_it_on_accept() {
        let tmp = site();
        let conductor = start(&tmp);
        let file = "content/post/empty.md";
        let result = conductor.handle_command(fill(
            file,
            "summary",
            "Suggested.",
            BlockEditIntent::Suggest,
        ));
        let Response::BlockEdited(BlockEditResponse::Done(BlockEditOutcome::SuggestionCreated {
            id,
        })) = result.response
        else {
            panic!("expected a suggestion, got {:?}", result.response)
        };
        assert!(
            slot_blocks(&conductor, file, "summary").is_empty(),
            "suggesting changes nothing"
        );
        let accepted = conductor.handle_command(Command::AcceptNedSuggestion { id });
        assert!(
            matches!(accepted.response, Response::Ok),
            "{:?}",
            accepted.response
        );
        assert_eq!(
            slot_blocks(&conductor, file, "summary"),
            [content::ContentElement::paragraph_from_markdown(
                "Suggested."
            )]
        );
    }

    /// A fill suggested while the slot was empty, accepted after the slot was
    /// filled another way, goes Stale as `Drifted`: the slot is not appended to.
    #[test]
    fn accepting_a_fill_after_the_slot_was_filled_is_drifted() {
        let tmp = site();
        let conductor = start(&tmp);
        let file = "content/post/empty.md";
        let suggested = conductor.handle_command(fill(
            file,
            "summary",
            "Suggested.",
            BlockEditIntent::Suggest,
        ));
        let Response::BlockEdited(BlockEditResponse::Done(BlockEditOutcome::SuggestionCreated {
            id,
        })) = suggested.response
        else {
            panic!("expected a suggestion, got {:?}", suggested.response)
        };
        let other = conductor.handle_command(fill(
            file,
            "summary",
            "Filled another way.",
            BlockEditIntent::Apply,
        ));
        assert!(applied(&other), "{:?}", other.response);

        let accepted = conductor.handle_command(Command::AcceptNedSuggestion { id: id.clone() });
        assert!(
            matches!(accepted.response, Response::Ok),
            "{:?}",
            accepted.response
        );
        let status = conductor
            .ned_suggestions
            .read()
            .unwrap()
            .get(&id)
            .expect("suggestion")
            .status
            .clone();
        assert!(
            matches!(
                status,
                editorial_types::NedSuggestionStatus::Stale {
                    cause: editorial_types::StaleCause::Drifted,
                    ..
                }
            ),
            "{status:?}"
        );
        assert_eq!(
            slot_blocks(&conductor, file, "summary"),
            [content::ContentElement::paragraph_from_markdown(
                "Filled another way."
            )],
            "the slot was not appended to"
        );
    }

    /// A slot that already has blocks is not filled: the client's empty base is
    /// out of date.
    #[test]
    fn block_edit_on_a_slot_that_has_blocks_is_refused_as_drifted() {
        let tmp = site();
        let conductor = start(&tmp);
        let result = conductor.handle_command(fill(
            "content/post/full.md",
            "summary",
            "Other.",
            BlockEditIntent::Apply,
        ));
        let Response::BlockEdited(BlockEditResponse::Refused(e)) = result.response else {
            panic!("expected a refusal, got {:?}", result.response)
        };
        assert_eq!(e.cause, BlockEditCause::Drifted, "{e:?}");
        assert_eq!(
            slot_blocks(&conductor, "content/post/full.md", "summary"),
            [content::ContentElement::paragraph_from_markdown(
                "A summary."
            )]
        );
    }
}

/// ADR-059 section 10: every address the server emits resolves to the block
/// that carries it, and the element's markdown is that block's markdown.
#[cfg(test)]
mod emitted_address_tests {
    use super::*;

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_dir(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// A copy of the example site (`site/`): content, schemas, templates.
    fn example_site() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site");
        for dir in ["content", "schemas", "templates"] {
            copy_dir(&source.join(dir), &tmp.path().join(dir));
        }
        tmp
    }

    /// The value of attribute `name` in a start tag, HTML-unescaped.
    fn attr(tag: &str, name: &str) -> Option<String> {
        let needle = format!(" {name}=\"");
        let rest = &tag[tag.find(&needle)? + needle.len()..];
        let value = &rest[..rest.find('"')?];
        Some(
            value
                .replace("&quot;", "\"")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        )
    }

    /// The store kind a rendered element stands for.
    fn kind_of(tag: &str) -> &'static str {
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match name {
            "p" => "paragraph",
            "pre" => "code-block",
            "blockquote" => "blockquote",
            "img" => "image",
            "table" => "table",
            "ul" | "ol" => "list",
            // The raw HTML block's placeholder (not a slot, so only its
            // address says it is a block).
            "div" if tag.contains("presemble-html-removed") => "html-block",
            "span" if tag.contains("hidden=") => "separator",
            // A synthesized title link stands for the title heading.
            "a" if tag.contains("data-presemble-source-slot=\"title\"") => "heading",
            "a" => "link",
            h if h.len() == 2 && h.starts_with('h') => "heading",
            // Slot elements rendered with another tag (`as="span"`...) carry
            // their kind in the address.
            _ => "",
        }
    }

    /// The home page lists features and posts with `data-each`; each item's
    /// title, summary and page link is editable from there, so each carries the
    /// address of its own document's block.
    #[test]
    fn data_each_items_carry_their_documents_addresses() {
        let tmp = example_site();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        let conductor = Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor");
        let html = conductor
            .render_page("/", &RenderMode::View)
            .expect("home page");
        let mut seen = std::collections::BTreeSet::new();
        for chunk in html.split('<').skip(1) {
            let Some(tag) = chunk.split('>').next() else {
                continue;
            };
            let file = attr(tag, "data-presemble-file").unwrap_or_default();
            let slot = attr(tag, "data-presemble-slot").unwrap_or_default();
            if !(file.starts_with("content/feature/") || file.starts_with("content/post/")) {
                continue;
            }
            let address = attr(tag, "data-presemble-address")
                .unwrap_or_else(|| panic!("<{tag}> of {file} carries no address"));
            assert!(
                address.contains(&format!("(ned/doc-by-path \"{file}\")")),
                "{address}"
            );
            // Titles and their page links address the title heading; text slots their own.
            if slot == "title" || slot == "link" {
                assert!(address.contains("\"title\")) \"heading\" 0)"), "{address}");
            }
            let selection = conductor.eval_selection(&address).expect("evaluates");
            assert_eq!(selection.len(), 1, "{address}");
            seen.insert((file, slot));
        }
        let slots: std::collections::BTreeSet<_> = seen.iter().map(|(_, s)| s.as_str()).collect();
        assert!(
            slots.contains("title") && slots.contains("link"),
            "{slots:?}"
        );
        assert!(seen.len() >= 4, "only {seen:?}");
    }

    /// The user guide's original `juxt` row: a table cell that ends in a link.
    /// (The guide had to avoid it: the emitted addresses did not resolve.)
    #[test]
    fn a_link_alone_in_a_table_cell_has_an_address_that_resolves() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["schemas/guide", "templates/guide", "content/guide"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("schemas/guide/item.md"),
            "# Guide title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\n\nBody content.\nheadings\n: h2..h6\n",
        )
        .unwrap();
        std::fs::write(
            root.join("templates/guide/item.hiccup"),
            "[:main [:presemble/insert {:data input.title :as \"h1\"}] [:presemble/insert {:data input.body}]]",
        )
        .unwrap();
        std::fs::write(
            root.join("content/guide/g.md"),
            "# The guide\n\n----\n\n## Template composition\n\nText.\n\n## Functions\n\n| Function | Meaning |\n| --- | --- |\n| `juxt` | Several templates side by side, used as `((juxt a b) input)`, see [Template composition](#template-composition) |\n| `pipe` | Plain text |\n\nAfter.\n",
        )
        .unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");
        let html = conductor
            .render_page("/guide/g", &RenderMode::View)
            .expect("page");

        let mut checked = 0;
        let mut saw_table = false;
        for chunk in html.split('<').skip(1) {
            let Some(tag) = chunk.split('>').next() else {
                continue;
            };
            saw_table |= tag.starts_with("table");
            let Some(address) = attr(tag, "data-presemble-address") else {
                continue;
            };
            checked += 1;
            let selection = conductor
                .eval_selection(&address)
                .unwrap_or_else(|e| panic!("<{tag}>: {address} does not evaluate: {e:?}"));
            assert_eq!(
                selection.len(),
                1,
                "<{tag}>: {address} resolves to {} nodes",
                selection.len()
            );
            let store = conductor.node_store.read().unwrap();
            let element = node_store_bridge::content_bridge::block_from_node(
                &store,
                selection.iter().next().unwrap(),
            )
            .unwrap_or_else(|e| panic!("<{tag}>: {address} is not a block: {e}"));
            let kind = content::address::kind_name(&element);
            let rendered = if attr(tag, "data-presemble-slot").as_deref() == Some("body") {
                kind_of(tag)
            } else {
                ""
            };
            assert!(
                rendered.is_empty() || rendered == kind,
                "<{tag}> resolves to a {kind}: {address}"
            );
            let projection = if address.contains("\"body\"))") {
                content::Projection::Block
            } else {
                content::Projection::Content
            };
            assert_eq!(
                content::project_markdown(&element, projection),
                attr(tag, "data-presemble-md"),
                "md of <{tag}>"
            );
        }
        assert!(saw_table, "{html}");
        assert!(checked >= 5, "only {checked} addresses emitted: {html}");
    }

    #[test]
    fn every_emitted_address_resolves_to_its_block() {
        let tmp = example_site();
        // A raw HTML block: its placeholder is addressed too.
        let guide = tmp.path().join("content/guide/getting-started.md");
        let mut text = std::fs::read_to_string(&guide).expect("the guide");
        text.push_str("\n<div>raw</div>\n");
        std::fs::write(&guide, text).unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(tmp.path())
            .build();
        let conductor = Conductor::with_repo(tmp.path().to_path_buf(), repo).expect("conductor");

        let mut urls: Vec<String> = conductor.url_to_root_index().into_keys().collect();
        urls.sort();
        let mut checked = 0;
        let mut with_md = 0;
        let mut editable_count = 0;
        let mut reference_links = 0;
        let mut placeholders = 0;
        let mut missing: Vec<String> = Vec::new();
        for url in urls {
            let Ok(html) = conductor.render_page(&url, &RenderMode::View) else {
                continue;
            };
            for chunk in html.split('<').skip(1) {
                let Some(tag) = chunk.split('>').next() else {
                    continue;
                };
                // Coverage: an element of a document's slot (it names the file
                // and the slot) must carry that block's address. A suggestion
                // placeholder renders no block.
                let editable = attr(tag, "data-presemble-file").is_some_and(|f| !f.is_empty())
                    && (attr(tag, "data-presemble-slot").is_some()
                        || attr(tag, "data-presemble-source-slot").is_some())
                    && attr(tag, "data-presemble-hint").is_none();
                if editable {
                    editable_count += 1;
                    if attr(tag, "data-presemble-address").is_none() {
                        // Accepted exclusion: a link slot of several blocks that
                        // resolve to other pages cannot be addressed exactly.
                        let file = attr(tag, "data-presemble-file").unwrap_or_default();
                        let slot = attr(tag, "data-presemble-slot").unwrap_or_default();
                        let slot_blocks = conductor.eval_selection(&format!(
                            "(ned/children (ned/slot (ned/doc-by-path {}) {}))",
                            content::address::clj_str(&file),
                            content::address::clj_str(&slot)
                        ));
                        let store = conductor.node_store.read().unwrap();
                        let is_link_slot = slot_blocks.is_ok_and(|s| {
                            s.len() > 1 && s.iter().all(|n| {
                                matches!(
                                    node_store_bridge::content_bridge::block_from_node(&store, n),
                                    Ok(ref el) if el.is_sole_link()
                                ) || matches!(
                                    node_store_bridge::content_bridge::block_from_node(&store, n),
                                    Ok(content::ContentElement::LinkExpression { .. })
                                )
                            })
                        });
                        if is_link_slot {
                            reference_links += 1;
                        } else {
                            missing.push(format!(
                                "{url}: <{}",
                                tag.chars().take(160).collect::<String>()
                            ));
                        }
                    }
                }
                let Some(address) = attr(tag, "data-presemble-address") else {
                    continue;
                };
                // A raw HTML block's placeholder is addressed but is no slot:
                // it names no file of its own, and is not counted as editable.
                let placeholder = tag.contains("presemble-html-removed");
                if placeholder {
                    placeholders += 1;
                } else {
                    checked += 1;
                    let file = attr(tag, "data-presemble-file").unwrap_or_default();
                    assert!(
                        address.contains(&format!("(ned/doc-by-path \"{file}\")")),
                        "{url}: the address names the file {file}: {address}"
                    );
                }

                // It resolves to exactly one node.
                let selection = conductor
                    .eval_selection(&address)
                    .unwrap_or_else(|e| panic!("{url}: {address} does not evaluate: {e:?}"));
                assert_eq!(
                    selection.len(),
                    1,
                    "{url}: {address} resolves to {} nodes",
                    selection.len()
                );
                let node = selection.iter().next().unwrap();

                // The node is the block that rendered it.
                let store = conductor.node_store.read().unwrap();
                let element = node_store_bridge::content_bridge::block_from_node(&store, node)
                    .unwrap_or_else(|e| panic!("{url}: {address} is not a block: {e}"));
                let kind = content::address::kind_name(&element);
                // Only a body block's tag says what it is; a template may render
                // a slot with any tag (`as="p"` for a heading slot).
                let rendered_kind =
                    if placeholder || attr(tag, "data-presemble-slot").as_deref() == Some("body") {
                        kind_of(tag)
                    } else {
                        ""
                    };
                assert!(
                    rendered_kind.is_empty() || rendered_kind == kind,
                    "{url}: <{tag}> resolves to a {kind}: {address}"
                );

                // The element's markdown is that block's markdown.
                let projection = if address.contains("\"body\"))") {
                    content::Projection::Block
                } else {
                    content::Projection::Content
                };
                if let Some(md) = attr(tag, "data-presemble-md") {
                    with_md += 1;
                    assert_eq!(
                        content::project_markdown(&element, projection),
                        Some(md),
                        "{url}: md of <{tag}>"
                    );
                } else {
                    assert_eq!(
                        content::project_markdown(&element, projection),
                        None,
                        "{url}: <{tag}> has no md but its block has some"
                    );
                }
            }
        }
        assert!(checked > 20, "only {checked} addresses were emitted");
        assert!(
            placeholders >= 1,
            "the raw HTML block's placeholder carries an address"
        );
        assert!(with_md > 20, "only {with_md} elements carried md");
        assert!(
            missing.is_empty(),
            "{} of {editable_count} editable elements have no address ({checked} have one):\n{}",
            missing.len(),
            missing
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
        // Every editable element is covered, but the multi-value link slots.
        assert_eq!(checked, editable_count - reference_links);
        assert_eq!(
            reference_links, 0,
            "site/ has no multi-value reference link slot rendered"
        );
    }
}

/// A client path never leaves the site directory (reads or writes).
///
/// Every test lays out `<tmp>/site/` (the site) beside `<tmp>/outside.md` and
/// `<tmp>/outside-dir/`, tries the attack, and checks that what is outside is
/// unchanged. A path the boundary refuses to build (`SitePath::parse`) cannot
/// be sent: that is the first line of defence and the attack stops there; the
/// conductor's own checks are the second, shown by the symlink tests, which
/// build a lexically valid path.
#[cfg(test)]
mod path_boundary_tests {
    use super::*;
    use editorial_types::{ContentPath, SitePath};

    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = "[:div [:h1 title]]";
    const OUTSIDE: &str = "# Outside\n\n----\n\nThe original text.\n";
    const BUFFER: &str = "# Overwritten\n\n----\n\nThis must never reach the file.\n";

    struct World {
        tmp: tempfile::TempDir,
        conductor: Conductor,
    }

    impl World {
        fn site(&self) -> PathBuf {
            self.tmp.path().join("site")
        }
        fn outside_file(&self) -> PathBuf {
            self.tmp.path().join("outside.md")
        }
        fn outside_dir(&self) -> PathBuf {
            self.tmp.path().join("outside-dir")
        }
        fn outside_bytes(&self) -> Vec<u8> {
            std::fs::read(self.outside_file()).expect("outside.md exists")
        }
    }

    fn world() -> World {
        let tmp = tempfile::tempdir().expect("tempdir");
        let site = tmp.path().join("site");
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(site.join(dir)).unwrap();
        }
        std::fs::write(site.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(site.join("templates/post/item.hiccup"), TEMPLATE).unwrap();
        std::fs::write(
            site.join("content/post/hello.md"),
            "# Hello\n\n----\n\nBody.\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("outside.md"), OUTSIDE).unwrap();
        std::fs::create_dir_all(tmp.path().join("outside-dir")).unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(&site)
            .build();
        let conductor = Conductor::with_repo(site, repo).expect("conductor");
        World { tmp, conductor }
    }

    /// The ways a client may spell "the file beside the site".
    fn spellings_of_outside(w: &World) -> Vec<String> {
        vec![
            w.outside_file().to_string_lossy().into_owned(),
            "../outside.md".to_string(),
            "content/../../outside.md".to_string(),
            "content/post/../../../outside.md".to_string(),
        ]
    }

    fn response_is_error(r: &Response) -> bool {
        matches!(r, Response::Error(_))
    }

    /// THE write: an editor buffer with a path outside the site, then a save.
    #[test]
    fn editor_buffer_outside_the_site_is_never_saved() {
        let w = world();
        let before = w.outside_bytes();
        for spelling in spellings_of_outside(&w) {
            let Ok(path) = ContentPath::parse(&spelling) else {
                continue;
            };
            w.conductor.handle_command(Command::EditorChanged {
                path,
                text: BUFFER.to_string(),
            });
            w.conductor.handle_command(Command::SaveAllBuffers {
                consent: Default::default(),
            });
            assert_eq!(
                w.outside_bytes(),
                before,
                "{spelling} overwrote a file outside the site"
            );
        }
        assert_eq!(w.outside_bytes(), before);
    }

    #[test]
    fn save_buffer_with_an_absolute_path_writes_nothing_outside() {
        let w = world();
        let before = w.outside_bytes();
        let spelling = w.outside_file().to_string_lossy().into_owned();
        if let Ok(path) = SitePath::parse(&spelling) {
            if let Ok(content_path) = ContentPath::parse(&spelling) {
                w.conductor.handle_command(Command::EditorChanged {
                    path: content_path,
                    text: BUFFER.to_string(),
                });
            }
            let saved = w.conductor.handle_command(Command::SaveBuffer {
                path,
                consent: Default::default(),
            });
            assert!(
                response_is_error(&saved.response),
                "saving outside the site must be refused: {:?}",
                saved.response
            );
        }
        assert_eq!(
            w.outside_bytes(),
            before,
            "an absolute path wrote outside the site"
        );
    }

    #[test]
    fn save_buffer_with_parent_segments_writes_nothing_outside() {
        let w = world();
        let before = w.outside_bytes();
        for spelling in [
            "../outside.md",
            "content/../../outside.md",
            "content/post/../../../outside.md",
        ] {
            if let Ok(path) = SitePath::parse(spelling) {
                if let Ok(content_path) = ContentPath::parse(spelling) {
                    w.conductor.handle_command(Command::EditorChanged {
                        path: content_path,
                        text: BUFFER.to_string(),
                    });
                }
                let saved = w.conductor.handle_command(Command::SaveBuffer {
                    path,
                    consent: Default::default(),
                });
                assert!(
                    response_is_error(&saved.response),
                    "{spelling}: saving outside the site must be refused: {:?}",
                    saved.response
                );
            }
            assert_eq!(
                w.outside_bytes(),
                before,
                "{spelling} wrote outside the site"
            );
        }
    }

    /// THE read: asking for the text of a file outside the site.
    #[test]
    fn get_document_text_cannot_read_outside_the_site() {
        let w = world();
        for spelling in spellings_of_outside(&w) {
            let Ok(path) = SitePath::parse(&spelling) else {
                continue;
            };
            let answer = w
                .conductor
                .handle_command(Command::GetDocumentText { path })
                .response;
            assert!(
                matches!(answer, Response::DocumentText(None)),
                "{spelling} read a file outside the site: {answer:?}"
            );
        }
    }

    #[test]
    fn cursor_moved_cannot_read_outside_the_site() {
        let w = world();
        // `content/post/` names the `post` schema, so a read would parse and
        // answer with an anchor.
        for spelling in ["content/post/../../../outside.md"] {
            let Ok(path) = SitePath::parse(spelling) else {
                continue;
            };
            let result = w
                .conductor
                .handle_command(Command::CursorMoved { path, line: 4 });
            assert!(
                result.events.is_empty(),
                "{spelling} read a file outside the site: {:?}",
                result.events
            );
        }
    }

    #[test]
    fn reload_refuses_a_path_outside_the_site() {
        let w = world();
        for spelling in [
            "content/../../outside.md",
            "content/post/../../../outside.md",
        ] {
            let Ok(path) = ContentPath::parse(spelling) else {
                continue;
            };
            let result = w.conductor.handle_command(Command::ReloadDocument { path });
            assert!(
                response_is_error(&result.response),
                "{spelling} must be refused: {:?}",
                result.response
            );
        }
    }

    /// A symlink inside the site that points out of it. The path is lexically
    /// fine, so only the conductor's own check can stop it.
    #[cfg(unix)]
    fn link_out(w: &World) {
        std::os::unix::fs::symlink(w.outside_dir(), w.site().join("content/escape"))
            .expect("symlink");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_site_pointing_out_is_not_written_through() {
        let w = world();
        link_out(&w);
        let path = SitePath::parse("content/escape/note.md").expect("a plain site path");
        w.conductor.handle_command(Command::EditorChanged {
            path: ContentPath::parse("content/escape/note.md").expect("a content path"),
            text: BUFFER.to_string(),
        });
        w.conductor.handle_command(Command::SaveBuffer {
            path,
            consent: Default::default(),
        });
        w.conductor.handle_command(Command::SaveAllBuffers {
            consent: Default::default(),
        });
        assert!(
            !w.outside_dir().join("note.md").exists(),
            "the buffer was written through a symlink out of the site"
        );
    }

    /// GUARD: a content path the type accepts, that is a symlink out of the
    /// site, is refused by the editor commands too: the type cannot see it,
    /// only `resolve_in_site` can.
    #[cfg(unix)]
    #[test]
    fn editor_commands_refuse_a_content_symlink_out_of_the_site() {
        let w = world();
        std::fs::write(
            w.outside_dir().join("secret.md"),
            "# Secret\n\n----\n\nBody.\n",
        )
        .unwrap();
        // Under `content/post/`, which names the `post` schema, so a read would
        // parse and answer with an anchor.
        std::os::unix::fs::symlink(w.outside_dir(), w.site().join("content/post/escape"))
            .expect("symlink");
        let spelling = "content/post/escape/secret.md";
        let content_path = ContentPath::parse(spelling).expect("a valid content path");

        for command in [
            Command::EditorOpened {
                path: content_path.clone(),
                text: BUFFER.to_string(),
            },
            Command::EditorChanged {
                path: content_path.clone(),
                text: BUFFER.to_string(),
            },
        ] {
            let answer = w.conductor.handle_command(command).response;
            assert!(response_is_error(&answer), "must be refused: {answer:?}");
        }
        assert!(
            w.conductor.editors.read().unwrap().is_empty(),
            "no editor was attached"
        );

        let moved = w.conductor.handle_command(Command::CursorMoved {
            path: SitePath::parse(spelling).expect("a plain site path"),
            line: 4,
        });
        assert!(
            moved.events.is_empty(),
            "read through a symlink out of the site: {:?}",
            moved.events
        );
        assert_eq!(
            std::fs::read_to_string(w.outside_dir().join("secret.md")).unwrap(),
            "# Secret\n\n----\n\nBody.\n",
            "nothing was written through the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_site_pointing_out_is_not_read_through() {
        let w = world();
        std::fs::write(w.outside_dir().join("secret.md"), OUTSIDE).unwrap();
        link_out(&w);
        let path = SitePath::parse("content/escape/secret.md").expect("a plain site path");
        let answer = w
            .conductor
            .handle_command(Command::GetDocumentText { path })
            .response;
        assert!(
            matches!(answer, Response::DocumentText(None)),
            "read through a symlink out of the site: {answer:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_changed_outside_the_site_is_ignored() {
        let w = world();
        link_out(&w);
        std::fs::write(w.outside_dir().join("note.md"), OUTSIDE).unwrap();
        // The reference: a change inside the site is a rebuild with events.
        let inside = w.conductor.handle_command(Command::FileChanged {
            paths: vec![SitePath::parse("content/post/hello.md").expect("valid")],
        });
        assert!(
            !inside.events.is_empty(),
            "the reference change must rebuild: {:?}",
            inside.events
        );

        // A change that resolves outside the site is not the conductor's.
        let outside = w.conductor.handle_command(Command::FileChanged {
            paths: vec![SitePath::parse("content/escape/note.md").expect("a plain site path")],
        });
        assert!(
            matches!(outside.response, Response::Ok),
            "{:?}",
            outside.response
        );
        assert!(
            outside.events.is_empty(),
            "an outside change was acted on: {:?}",
            outside.events
        );
    }

    /// GUARD: a block edit for a file outside the site was already refused (the
    /// selection resolves into another document). It must stay refused.
    #[test]
    fn block_edit_with_a_traversal_file_is_refused() {
        let w = world();
        let Ok(file) = ContentPath::parse("content/../../outside.md") else {
            return;
        };
        let result = w.conductor.handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file,
            selection: r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/post/hello.md") "body")) "paragraph" 0)"#.to_string(),
            base: editorial_types::BlockText::Markdown("Body.".to_string()),
            proposed: editorial_types::BlockText::Markdown("Changed.".to_string()),
            intent: editorial_types::BlockEditIntent::Apply,
            reason: None,
        }));
        assert!(
            matches!(
                result.response,
                Response::BlockEdited(editorial_types::BlockEditResponse::Refused(_))
            ),
            "{:?}",
            result.response
        );
        assert_eq!(w.outside_bytes(), OUTSIDE.as_bytes());
    }

    /// GUARD: as above, for a suggestion.
    #[test]
    fn create_ned_suggestion_with_a_traversal_file_is_refused() {
        let w = world();
        let Ok(file) = ContentPath::parse("content/../../outside.md") else {
            return;
        };
        let result = w.conductor.handle_command(Command::CreateNedSuggestion {
            file,
            selection: r#"(ned/text-in (ned/children (ned/slot (ned/doc-by-path "content/post/hello.md") "title")))"#.to_string(),
            mutation: editorial_types::NedMutation::SetText("Hijacked".to_string()),
            reason: "traversal".to_string(),
            author: editorial_types::Author::Claude,
        });
        assert!(response_is_error(&result.response), "{:?}", result.response);
        assert_eq!(w.outside_bytes(), OUTSIDE.as_bytes());
    }

    // ── URL paths and schema stems (commit 2) ────────────────────────────────

    /// Output layout of the world: `<tmp>/output-editorial/site/` is the
    /// conductor's output directory; `<tmp>/output-editorial/` (its parent) is
    /// outside it.
    fn write_output(w: &World, rel: &str, text: &str) {
        let path = w.tmp.path().join("output-editorial").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn render_view(w: &World, url: &str) -> Response {
        w.conductor
            .handle_command(Command::RenderPage {
                path: url.to_string(),
                mode: RenderMode::View,
            })
            .response
    }

    #[test]
    fn render_with_a_traversal_url_reads_nothing_outside_output() {
        let w = world();
        write_output(&w, "site/post/hello/index.html", "<p>hello</p>");
        // Beside the output directory, reached by `..` as the first segment.
        write_output(&w, "outside/index.html", "SECRET-PAGE");
        write_output(&w, "index.html", "SECRET-ROOT");

        // Control: a plain URL still renders.
        assert!(
            matches!(render_view(&w, "/post/hello"), Response::PageRendered { ref html } if html == "<p>hello</p>"),
            "the plain URL must still render"
        );
        for url in [
            "/../../outside",
            "/../outside/",
            "/../x/../outside",
            "/post/../../outside",
        ] {
            match render_view(&w, url) {
                Response::Error(_) => {}
                other => panic!("{url} must be refused, got {other:?}"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn render_through_a_symlink_out_of_output_reads_nothing() {
        let w = world();
        write_output(&w, "site/post/hello/index.html", "<p>hello</p>");
        std::fs::write(w.outside_dir().join("index.html"), "SECRET-LINK").unwrap();
        std::os::unix::fs::symlink(
            w.outside_dir(),
            w.tmp.path().join("output-editorial/site/escape"),
        )
        .expect("symlink");
        match render_view(&w, "/escape/") {
            Response::Error(_) => {}
            other => panic!("/escape/ must be refused, got {other:?}"),
        }
    }

    #[test]
    fn links_with_a_traversal_stem_reads_nothing() {
        let w = world();
        // Control: a known stem answers.
        let known = w.conductor.handle_command(Command::ListLinkOptions {
            stem: "post".to_string(),
        });
        assert!(
            matches!(known.response, Response::LinkOptions(_)),
            "{:?}",
            known.response
        );
        for stem in ["../../x", "../templates/post", "/etc", "nope", "post/../x"] {
            let result = w.conductor.handle_command(Command::ListLinkOptions {
                stem: stem.to_string(),
            });
            assert!(
                response_is_error(&result.response),
                "stem {stem:?} must be refused: {:?}",
                result.response
            );
        }
    }

    /// GUARD: `GetGrammar` only looks the stem up in the schema cache; no path
    /// is built from it, and an unknown stem already answers "not found".
    #[test]
    fn grammar_with_an_unknown_stem_is_an_error() {
        let w = world();
        for stem in ["../../x", "../schemas/post/item", "/etc/passwd", "nope"] {
            let result = w.conductor.handle_command(Command::GetGrammar {
                stem: stem.to_string(),
            });
            assert!(
                matches!(result.response, Response::SchemaSource(None)),
                "stem {stem:?} must be not found: {:?}",
                result.response
            );
        }
        let known = w.conductor.handle_command(Command::GetGrammar {
            stem: "post".to_string(),
        });
        assert!(matches!(known.response, Response::SchemaSource(Some(_))));
    }

    /// GUARD: the schema render only gets as far as the template files for a
    /// stem that has a schema; a traversal stem has none.
    #[test]
    fn schema_render_with_a_traversal_stem_is_an_error() {
        let w = world();
        for url in [
            "/_schema/../item",
            "/_schema/../index",
            "/_schema/nope/item",
        ] {
            let result = w.conductor.handle_command(Command::RenderPage {
                path: url.to_string(),
                mode: RenderMode::Schema,
            });
            assert!(
                response_is_error(&result.response),
                "{url}: {:?}",
                result.response
            );
        }
    }

    /// GUARD: both are lookups in maps built from the site.
    #[test]
    fn schema_for_and_page_for_with_traversal_input_answer_none() {
        let w = world();
        let page = w.conductor.handle_command(Command::SchemaUrlForPage {
            page_url: "/../../outside".to_string(),
        });
        assert!(
            matches!(page.response, Response::SchemaUrl(None)),
            "{:?}",
            page.response
        );
        for schema_url in [
            "/_schema/../item",
            "/_schema/../../x/item",
            "/_schema/nope/item",
        ] {
            let result = w.conductor.handle_command(Command::PageUrlForSchema {
                schema_url: schema_url.to_string(),
            });
            assert!(
                matches!(result.response, Response::PageUrl(None)),
                "{schema_url}: {:?}",
                result.response
            );
        }
    }

    /// GUARD: a slug is a name, never a path.
    #[test]
    fn create_content_refuses_a_traversal_slug() {
        let w = world();
        for slug in ["../../outside-dir/evil", "../evil", "/tmp/evil"] {
            let result = w.conductor.handle_command(Command::CreateContent {
                stem: "post".to_string(),
                slug: slug.to_string(),
            });
            assert!(
                response_is_error(&result.response),
                "{slug}: {:?}",
                result.response
            );
        }
        assert!(!w.outside_dir().join("evil.md").exists());
        assert!(!w.tmp.path().join("evil.md").exists());
        assert!(!w.site().join("content/evil.md").exists());
    }
}
