//! Editorial pages carry their diagnostics where they arise (FU-7b).
//!
//! The conductor never validates pages from disk: N is what the editorial view
//! shows. So an editorial render validates N's document (rebuilt from the
//! store, with the page's grammar) and puts each diagnostic on the smallest
//! element it is about, by the structural [`content::Location`] the validator
//! reports: the top-level block by its `data-presemble-address`, and a block
//! nested in a quote or a list item by its `data-presemble-path`. What belongs
//! to a slot goes on the slot's element (a missing slot renders a placeholder
//! there). What no element can carry (unassigned content, which is never
//! rendered) is listed on the page: a `<script type="application/json"
//! id="presemble-diagnostics">` with the counts of every diagnostic of the
//! page, and the unplaced ones. Messages already quote their element; N has
//! no lines, so no line is given.

use super::*;
use crate::protocol::ConflictRegion;
use content::{
    AddressContainer, ContentElement, Document, Location, NestedStep, Severity,
    ValidationDiagnostic,
};
use node_store::{NodeId, NodeStore};
use serde_json::{Value, json};
use template::constants::{ATTR_ADDRESS, ATTR_PATH, ATTR_SLOT};
use template::dom::{Element, Form, Node};

/// The attribute that carries an element's diagnostics: a JSON array of
/// `{"severity", "message"}`.
const ATTR_DIAGNOSTICS: &str = "data-presemble-diagnostics";

/// The attribute that marks a block that conflicts with its file on disk: a
/// JSON array of `{"id", "file", "disk", "message"}`.
const ATTR_CONFLICT: &str = "data-presemble-conflict";

const CONFLICT_MESSAGE: &str = "This block changed on disk differently from the unsaved edits. Take the file's version of this block, or keep the unsaved version.";

/// Where a diagnostic is shown.
enum Target {
    /// A block: the top-level element with this address, and inside it the
    /// element with this path (the nearest one on the way, if the block itself
    /// renders none).
    Block {
        address: String,
        path: Vec<NestedStep>,
    },
    /// The first element of a slot.
    Slot(String),
    /// No element: the page lists it.
    Unplaced,
}

fn severity_name(severity: &Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    }
}

fn diagnostic_json(d: &ValidationDiagnostic) -> Value {
    json!({ "severity": severity_name(&d.severity), "message": d.message })
}

/// `i1/b0` for `[Item(1), Block(0)]`: how a nested block is named on the page.
fn path_text(path: &[NestedStep]) -> String {
    path.iter()
        .map(|step| match step {
            NestedStep::Block(n) => format!("b{n}"),
            NestedStep::Item(n) => format!("i{n}"),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Where a diagnostic is shown: its location, or else its slot.
fn target(
    d: &ValidationDiagnostic,
    doc: &Document,
    file: &str,
    addresses: &mut HashMap<String, Vec<Option<String>>>,
) -> Target {
    match &d.location {
        Some(location) => target_of(location, doc, file, addresses),
        None => d
            .slot
            .as_ref()
            .map(|s| Target::Slot(s.as_str().to_string()))
            .unwrap_or(Target::Unplaced),
    }
}

/// Where the block at `location` of `doc` is shown.
fn target_of(
    location: &Location,
    doc: &Document,
    file: &str,
    addresses: &mut HashMap<String, Vec<Option<String>>>,
) -> Target {
    let Location {
        container,
        index,
        path,
    } = location;
    let (key, container_address, elements) = match container {
        content::Container::Body => ("body".to_string(), AddressContainer::Body, &doc.body),
        content::Container::Slot(name) => {
            let Some(slot) = doc.preamble.iter().find(|s| s.name == *name) else {
                return Target::Unplaced;
            };
            (
                name.as_str().to_string(),
                AddressContainer::Slot(name.as_str()),
                &slot.elements,
            )
        }
        // Never rendered.
        content::Container::Unassigned => return Target::Unplaced,
    };
    let of_container = addresses.entry(key).or_insert_with(|| {
        let blocks: Vec<ContentElement> = elements.iter().map(|e| e.node.clone()).collect();
        content::block_addresses(file, container_address, &blocks)
    });
    match of_container.get(*index).cloned().flatten() {
        Some(address) => Target::Block {
            address,
            path: path.clone(),
        },
        // A block with no address (a link expression, an ambiguous heading)
        // renders no element of its own: its slot, if it has one.
        None => match container {
            content::Container::Slot(name) => Target::Slot(name.as_str().to_string()),
            _ => Target::Unplaced,
        },
    }
}

/// The indices, from `nodes` down, of the first element (in document order)
/// that satisfies `matches`.
fn locate(nodes: &[Node], matches: &dyn Fn(&Element) -> bool) -> Option<Vec<usize>> {
    for (i, node) in nodes.iter().enumerate() {
        if let Node::Element(element) = node {
            if matches(element) {
                return Some(vec![i]);
            }
            if let Some(mut below) = locate(&element.children, matches) {
                below.insert(0, i);
                return Some(below);
            }
        }
    }
    None
}

fn element_at<'a>(nodes: &'a [Node], at: &[usize]) -> Option<&'a Element> {
    let (first, rest) = at.split_first()?;
    let Node::Element(element) = nodes.get(*first)? else {
        return None;
    };
    if rest.is_empty() {
        Some(element)
    } else {
        element_at(&element.children, rest)
    }
}

fn element_at_mut<'a>(nodes: &'a mut [Node], at: &[usize]) -> Option<&'a mut Element> {
    let (first, rest) = at.split_first()?;
    let Node::Element(element) = nodes.get_mut(*first)? else {
        return None;
    };
    if rest.is_empty() {
        Some(element)
    } else {
        element_at_mut(&mut element.children, rest)
    }
}

fn has_attr(element: &Element, name: &str, value: &str) -> bool {
    element.attr(name) == Some(value)
}

/// Where in the rendered nodes a target is, if anywhere.
fn resolve(nodes: &[Node], target: &Target) -> Option<Vec<usize>> {
    match target {
        Target::Unplaced => None,
        Target::Slot(name) => locate(nodes, &|e| has_attr(e, ATTR_SLOT, name)),
        Target::Block { address, path } => {
            let top = locate(nodes, &|e| has_attr(e, ATTR_ADDRESS, address))?;
            let top_element = element_at(nodes, &top)?;
            // The block itself, or the nearest enclosing element that has a path.
            for depth in (1..=path.len()).rev() {
                let wanted = path_text(&path[..depth]);
                if let Some(below) =
                    locate(&top_element.children, &|e| has_attr(e, ATTR_PATH, &wanted))
                {
                    return Some(top.iter().copied().chain(below).collect());
                }
            }
            Some(top)
        }
    }
}

/// What a page's validation found, and the blocks of its document that
/// conflict with its file on disk (ADR-065 amendment F2).
pub(super) struct PageDiagnostics {
    doc: Document,
    file: String,
    diagnostics: Vec<ValidationDiagnostic>,
    conflicts: Vec<ConflictRegion>,
}

/// What a page can tell about a conflicted block: its id (to resolve it),
/// the file, the file's version of the block, and what is wrong.
fn conflict_json(file: &str, region: &ConflictRegion) -> Value {
    json!({
        "id": region.id.to_string(),
        "file": file,
        "disk": region.disk,
        "message": CONFLICT_MESSAGE,
    })
}

/// Put each item on the element its target names; return the ones no
/// element can carry. Each placed element gets `attr`: a JSON array of what
/// `to_json` makes of its items.
fn place<'a, T>(
    nodes: &mut [Node],
    items: impl IntoIterator<Item = (&'a T, Target)>,
    attr: &str,
    to_json: impl Fn(&T) -> Value,
) -> Vec<&'a T> {
    let mut unplaced = Vec::new();
    let mut placed: HashMap<Vec<usize>, Vec<&T>> = HashMap::new();
    for (item, target) in items {
        match resolve(nodes, &target) {
            Some(at) => placed.entry(at).or_default().push(item),
            None => unplaced.push(item),
        }
    }
    for (at, items) in placed {
        if let Some(element) = element_at_mut(nodes, &at) {
            let list: Vec<Value> = items.iter().map(|item| to_json(item)).collect();
            element
                .attrs
                .push((attr.to_string(), Form::Str(Value::Array(list).to_string())));
        }
    }
    unplaced
}

/// What [`PageDiagnostics::attach`] could not put on an element.
struct Unplaced<'a> {
    diagnostics: Vec<&'a ValidationDiagnostic>,
    conflicts: Vec<&'a ConflictRegion>,
}

impl PageDiagnostics {
    /// Put every diagnostic and every conflicted block on the element it is
    /// about; return the ones no element can carry.
    fn attach(&self, nodes: &mut [Node]) -> Unplaced<'_> {
        let mut addresses = HashMap::new();
        let diagnostics: Vec<_> = self
            .diagnostics
            .iter()
            .map(|d| (d, target(d, &self.doc, &self.file, &mut addresses)))
            .collect();
        let diagnostics = place(nodes, diagnostics, ATTR_DIAGNOSTICS, diagnostic_json);
        let conflicts: Vec<_> = self
            .conflicts
            .iter()
            .map(|region| {
                let target = match &region.location {
                    Some(location) => target_of(
                        &Location::from(location),
                        &self.doc,
                        &self.file,
                        &mut addresses,
                    ),
                    None => Target::Unplaced,
                };
                (region, target)
            })
            .collect();
        let conflicts = place(nodes, conflicts, ATTR_CONFLICT, |region| {
            conflict_json(&self.file, region)
        });
        Unplaced {
            diagnostics,
            conflicts,
        }
    }

    /// The page script: the page's own document (`file`, site-relative: what
    /// its save state and leave hint are about, ADR-070), the counts of every
    /// diagnostic, the unplaced ones, and the conflicted blocks no element
    /// carries.
    fn script(&self, unplaced: &Unplaced) -> String {
        let count = |severity: Severity| {
            self.diagnostics
                .iter()
                .filter(|d| d.severity == severity)
                .count()
        };
        let page = json!({
            "file": self.file,
            "errors": count(Severity::Error),
            "warnings": count(Severity::Warning),
            "unplaced": unplaced.diagnostics.iter().map(|d| diagnostic_json(d)).collect::<Vec<_>>(),
            "conflicts": unplaced
                .conflicts
                .iter()
                .map(|region| conflict_json(&self.file, region))
                .collect::<Vec<_>>(),
        });
        // Inside a script element nothing is unescaped: keep `<` out of it.
        let text = page.to_string().replace('<', "\\u003c");
        format!("<script type=\"application/json\" id=\"presemble-diagnostics\">{text}</script>")
    }
}

impl Conductor {
    /// The page's document as N holds it, validated against the page's grammar.
    /// `None` when the document cannot be decoded.
    fn validate_page(&self, store: &NodeStore, doc_root: NodeId) -> Option<PageDiagnostics> {
        let doc = node_store_bridge::content_bridge::store_to_document(store, doc_root).ok()?;
        let find =
            |name: &str| node_store_bridge::content_bridge::find_attr_text(store, doc_root, name);
        let stem = find("stem")?;
        let file = find("file").unwrap_or_default();
        let key = if find("page-kind").as_deref() == Some("collection") {
            site_index::schema_cache_key(&stem, "index")
        } else {
            // The item schema, or the stem's own.
            let cache = self.schema_cache.read().unwrap_or_else(|e| e.into_inner());
            let item_key = format!("{stem}/item");
            if cache.contains_key(&item_key) {
                item_key
            } else {
                stem.clone()
            }
        };
        // A page with no schema (template-only) has nothing to validate.
        let diagnostics = match self.grammar_for(&key) {
            Some(grammar) => content::validate(&doc, &grammar).diagnostics,
            None => Vec::new(),
        };
        // The blocks that conflict with the file (a whole-file conflict has
        // none: the page notice tells of it). The page now shows the
        // conflict as it stands: it is no longer out of date.
        self.conflict_unrendered
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(Path::new(&file));
        let conflicts = self
            .dirty_docs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .conflict_regions(Path::new(&file))
            .map(<[ConflictRegion]>::to_vec)
            .unwrap_or_default();
        Some(PageDiagnostics {
            doc,
            file,
            diagnostics,
            conflicts,
        })
    }

    /// The grammar of the schema cached under `key`, parsed once per source.
    fn grammar_for(&self, key: &str) -> Option<Arc<schema::Grammar>> {
        let source = self
            .schema_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned()?;
        if let Some((parsed_from, grammar)) = self
            .parsed_grammars
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            && *parsed_from == source
        {
            return Some(grammar.clone());
        }
        let grammar = Arc::new(schema::parse_schema(&source).ok()?);
        self.parsed_grammars
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), (source, grammar.clone()));
        Some(grammar)
    }

    /// The editorial HTML of a rendered page: its nodes serialized, with the
    /// page's diagnostics attached where they arise, and the page script.
    pub(super) fn editorial_html(
        &self,
        store: &NodeStore,
        doc_root: NodeId,
        mut nodes: Vec<Node>,
    ) -> String {
        let Some(page) = self.validate_page(store, doc_root) else {
            return template::serialize_nodes(&nodes);
        };
        let unplaced = page.attach(&mut nodes);
        let mut html = template::serialize_nodes(&nodes);
        let script = page.script(&unplaced);
        match html.rfind("</body>") {
            Some(at) => html.insert_str(at, &script),
            None => html.push_str(&script),
        }
        html
    }
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::Value;

    const SCHEMA: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
    const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert><div><presemble:insert data="(ref input.body)"></presemble:insert></div></body></html>"#;

    fn conductor_with(content: &str) -> (crate::test_site::TestSite, Conductor) {
        let tmp = crate::test_site::TestSite::new();
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join("content/post/x.md"), content).unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");
        (tmp, conductor)
    }

    /// The editorial page of `content`.
    fn editorial_page(content: &str) -> String {
        let (_tmp, conductor) = conductor_with(content);
        conductor
            .render_page("/post/x", &RenderMode::View)
            .expect("rendered")
    }

    /// The opening tags in `html` that carry the attribute `name`.
    fn tags_with(html: &str, name: &str) -> Vec<String> {
        html.split('<')
            .filter_map(|part| part.split_once('>').map(|(tag, _)| tag))
            .filter(|tag| tag.contains(&format!("{name}=\"")))
            .map(|tag| format!("<{tag}>"))
            .collect()
    }

    fn unescape(attr: &str) -> String {
        attr.replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    /// The diagnostics an element carries.
    fn carried(tag: &str) -> Vec<Value> {
        let start = tag
            .find("data-presemble-diagnostics=\"")
            .expect("the attribute")
            + 28;
        let end = start + tag[start..].find('"').expect("closed");
        serde_json::from_str(&unescape(&tag[start..end])).expect("a JSON array")
    }

    /// The page script.
    fn page_script(html: &str) -> Value {
        let open = "<script type=\"application/json\" id=\"presemble-diagnostics\">";
        let start = html.find(open).expect("the page script") + open.len();
        let end = start + html[start..].find("</script>").expect("closed");
        serde_json::from_str(&html[start..end]).expect("JSON")
    }

    fn messages(diagnostics: &[Value]) -> Vec<String> {
        diagnostics
            .iter()
            .map(|d| d["message"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The page names its own document (ADR-070: what its save state and
    /// leave hint are about), even where the first element with a
    /// `data-presemble-file` belongs to another document: here a collection
    /// page lists its items above its own heading.
    #[test]
    fn the_page_script_names_the_pages_own_file() {
        let tmp = crate::test_site::TestSite::new();
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(
            root.join("schemas/post/index.md"),
            "# Listing title {#title}\noccurs\n: exactly once\n",
        )
        .unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(
            root.join("templates/post/index.hiccup"),
            r#"[:html [:body
  [:ul [:template {:data-each input.post} [:li [:presemble/insert {:data item.title :as "h2"}]]]]
  [:presemble/insert {:data input.title :as "h1"}]]]"#,
        )
        .unwrap();
        std::fs::write(root.join("content/post/index.md"), "# Posts\n").unwrap();
        std::fs::write(
            root.join("content/post/x.md"),
            "# First Item\n\n----\n\nBody.\n",
        )
        .unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let conductor = Conductor::with_repo(root.to_path_buf(), repo).expect("conductor");

        let html = conductor
            .render_page("/post/", &RenderMode::View)
            .expect("rendered");

        let first = html
            .split("data-presemble-file=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("an element with a file");
        assert_eq!(first, "content/post/x.md", "an item comes first: {html}");
        assert_eq!(
            page_script(&html)["file"],
            "content/post/index.md",
            "{html}"
        );
    }

    #[test]
    fn editorial_block_carries_its_errors() {
        let html = editorial_page("# Title\n\n----\n\nFine one.\n\nFine two <b>x</b>.\n");
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(tags[0].starts_with("<p "), "{}", tags[0]);
        assert!(
            tags[0].contains("&quot;paragraph&quot; 1"),
            "the second paragraph: {}",
            tags[0]
        );
        let diagnostics = carried(&tags[0]);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
        assert_eq!(diagnostics[0]["severity"], "error");
        assert!(
            messages(&diagnostics)[0].contains("inline HTML"),
            "{diagnostics:#?}"
        );
    }

    #[test]
    fn editorial_nested_block_carries_its_errors() {
        let html = editorial_page(
            "# Title\n\n----\n\nIntro.\n\n- fine\n\n- a <b>x</b> b\n\n> fine q\n>\n> q <i>y</i>\n",
        );
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 2, "{tags:#?}");
        let in_item = tags
            .iter()
            .find(|t| t.contains("data-presemble-path=\"i1/b0\""))
            .expect("the item paragraph");
        let in_quote = tags
            .iter()
            .find(|t| t.contains("data-presemble-path=\"b1\""))
            .expect("the quote paragraph");
        assert!(
            in_item.starts_with("<p ") && in_quote.starts_with("<p "),
            "{tags:#?}"
        );
        assert!(messages(&carried(in_item))[0].contains("inline HTML"));
        assert!(messages(&carried(in_quote))[0].contains("inline HTML"));
        // The list and the quote themselves carry nothing.
        assert!(
            tags.iter()
                .all(|t| !t.starts_with("<ul") && !t.starts_with("<blockquote")),
            "{tags:#?}"
        );
    }

    #[test]
    fn editorial_slot_placeholder_carries_the_missing_slot_error() {
        let html = editorial_page("----\n\nBody.\n");
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(
            tags[0].contains("data-presemble-slot=\"title\""),
            "the title placeholder: {}",
            tags[0]
        );
        assert!(tags[0].contains("data-presemble-hint"), "{}", tags[0]);
        let diagnostics = carried(&tags[0]);
        assert!(
            messages(&diagnostics)
                .iter()
                .any(|m| m.contains("missing required heading")),
            "{diagnostics:#?}"
        );
        assert!(
            page_script(&html)["unplaced"]
                .as_array()
                .expect("a list")
                .is_empty()
        );
    }

    #[test]
    fn editorial_page_lists_unassigned_content_and_unplaced_diagnostics() {
        let html = editorial_page("# Title\n\nStray paragraph.\n\n----\n\nBody.\n");
        assert!(
            tags_with(&html, "data-presemble-diagnostics").is_empty(),
            "nothing rendered carries it"
        );
        let script = page_script(&html);
        let unplaced = script["unplaced"].as_array().expect("a list");
        assert_eq!(unplaced.len(), 1, "{script}");
        assert_eq!(unplaced[0]["severity"], "error");
        assert!(
            unplaced[0]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("Stray paragraph"),
            "{script}"
        );
    }

    #[test]
    fn editorial_page_counts_errors_and_warnings() {
        // An unassigned paragraph (error, unplaced), inline HTML (error,
        // placed), and a link expression in the body (warning, renders nothing).
        let html = editorial_page(
            "# Title\n\nStray one.\n\n----\n\nFine <b>x</b>.\n\n[Posts](->> :post)\n",
        );
        let script = page_script(&html);
        assert_eq!(script["errors"], 2, "{script}");
        assert_eq!(script["warnings"], 1, "{script}");
        // Every diagnostic is counted, placed or not; the unplaced are listed.
        assert_eq!(
            script["unplaced"].as_array().expect("a list").len(),
            2,
            "{script}"
        );
        assert_eq!(tags_with(&html, "data-presemble-diagnostics").len(), 1);
    }

    /// Content is user text and messages quote it: a paragraph with a script
    /// end tag in it must not be able to end the page script (or the
    /// attribute) and run code in the editorial page.
    #[test]
    fn a_message_quoting_a_script_end_tag_cannot_break_out() {
        let html =
            editorial_page("# Title\n\n----\n\nFine </script><script>alert(1)</script> text.\n");

        // The page script: one end tag after its opening tag, and the JSON
        // reads back as the original message.
        let open = "<script type=\"application/json\" id=\"presemble-diagnostics\">";
        let after_open = &html[html.find(open).expect("the page script") + open.len()..];
        assert_eq!(
            after_open.matches("</script>").count(),
            1,
            "only ours ends the script: {after_open}"
        );
        assert_eq!(
            html.matches("</script>").count(),
            1,
            "no other end tag anywhere: {html}"
        );
        let script = page_script(&html);
        assert_eq!(script["errors"], 1, "{script}");

        // The element: the attribute parses back to the message, and the raw
        // HTML holds none of the quoted tags.
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(
            !tags[0].contains("</script>") && !tags[0].contains("<script>"),
            "{}",
            tags[0]
        );
        let message = carried(&tags[0])[0]["message"]
            .as_str()
            .expect("a message")
            .to_string();
        assert!(
            message.contains("</script><script>alert(1)</script>"),
            "{message}"
        );
        assert!(
            script["unplaced"].as_array().expect("a list").is_empty(),
            "placed on its paragraph: {script}"
        );
    }

    /// In a tight item the paragraph renders without an element of its own:
    /// its diagnostic belongs to the item.
    #[test]
    fn a_tight_items_paragraph_diagnostic_lands_on_its_item() {
        let html = editorial_page("# Title\n\n----\n\nIntro.\n\n- fine\n- a <b>x</b> b\n");
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(
            tags[0].starts_with("<li "),
            "the item, not the whole list: {}",
            tags[0]
        );
        assert!(
            tags[0].contains("data-presemble-path=\"i1\""),
            "{}",
            tags[0]
        );
        assert!(messages(&carried(&tags[0]))[0].contains("inline HTML"));
    }

    /// The raw HTML block's placeholder (`div.presemble-html-removed`) is the
    /// element its Error belongs to, not the panel.
    #[test]
    fn the_raw_html_placeholder_carries_its_error() {
        let html =
            editorial_page("# Title\n\n----\n\nBefore.\n\n<div class=\"x\">raw</div>\n\nAfter.\n");
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(
            tags[0].starts_with("<div ") && tags[0].contains("presemble-html-removed"),
            "the placeholder: {}",
            tags[0]
        );
        assert_eq!(carried(&tags[0])[0]["severity"], "error");
        let script = page_script(&html);
        assert!(
            script["unplaced"].as_array().expect("a list").is_empty(),
            "{script}"
        );
        assert_eq!(script["errors"], 1, "{script}");
    }

    /// The same inside a quote: the placeholder knows its path.
    #[test]
    fn a_nested_raw_html_placeholder_carries_its_error() {
        let html = editorial_page("# Title\n\n----\n\n> fine q\n>\n> <div>raw</div>\n");
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(
            tags[0].starts_with("<div ") && tags[0].contains("presemble-html-removed"),
            "the placeholder: {}",
            tags[0]
        );
        assert!(
            tags[0].contains("data-presemble-path=\"b1\""),
            "{}",
            tags[0]
        );
        assert!(
            page_script(&html)["unplaced"]
                .as_array()
                .expect("a list")
                .is_empty()
        );
    }

    /// GUARD: the placeholder is addressed (so its Error finds it) but is not
    /// a slot, so the browser cannot edit it (ADR-062 D4).
    #[test]
    fn the_raw_html_placeholder_is_not_editable() {
        let html = editorial_page("# Title\n\n----\n\n<div>raw</div>\n");
        let tags: Vec<String> = tags_with(&html, "class")
            .into_iter()
            .filter(|t| t.contains("presemble-html-removed"))
            .collect();
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(tags[0].contains("data-presemble-address=\""), "{}", tags[0]);
        assert!(tags[0].contains("html-block"), "{}", tags[0]);
        assert!(!tags[0].contains("data-presemble-slot"), "{}", tags[0]);
        assert!(!tags[0].contains("data-presemble-md"), "{}", tags[0]);
    }

    /// GUARD: published output holds the placeholder with no editor data.
    #[test]
    fn published_placeholder_carries_no_editor_attributes() {
        let (_tmp, conductor) =
            conductor_with("# Title\n\n----\n\n> q\n>\n> <div>in</div>\n\n<div>raw</div>\n");
        conductor.build_all_pages(template::OutputMode::Published);
        let path = site_index::output_path_for_stem_slug(&conductor.output_dir, "post", "x");
        let html = std::fs::read_to_string(path).expect("the page");
        let placeholders = tags_with(&html, "class");
        let placeholders: Vec<_> = placeholders
            .iter()
            .filter(|t| t.contains("presemble-html-removed"))
            .collect();
        assert_eq!(placeholders.len(), 2, "{html}");
        for tag in placeholders {
            assert!(
                !tag.contains("data-presemble-"),
                "no editor attribute: {tag}"
            );
        }
    }

    /// UM-1: markup that looks unfinished is marked on the element it is in.
    #[test]
    fn editorial_page_marks_unfinished_markup_on_its_element() {
        let html = editorial_page("# Title\n\n----\n\nFine.\n\nA **bold claim.\n");
        let tags = tags_with(&html, "data-presemble-diagnostics");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(tags[0].starts_with("<p "), "{}", tags[0]);
        assert!(
            tags[0].contains("&quot;paragraph&quot; 1"),
            "the second paragraph: {}",
            tags[0]
        );
        let diagnostics = carried(&tags[0]);
        assert_eq!(diagnostics[0]["severity"], "error");
        assert!(
            messages(&diagnostics)[0]
                .contains("looks like unfinished formatting (an unclosed bold)"),
            "{diagnostics:#?}"
        );
        assert_eq!(page_script(&html)["errors"], 1);
    }

    /// GUARD: a page with nothing to report has no attribute, and a page
    /// script with zero counts.
    #[test]
    fn a_valid_page_carries_no_diagnostics_attribute() {
        let html = editorial_page("# Title\n\n----\n\nFine.\n");
        assert!(tags_with(&html, "data-presemble-diagnostics").is_empty());
        let script = page_script(&html);
        assert_eq!(script["errors"], 0);
        assert_eq!(script["warnings"], 0);
        assert!(script["unplaced"].as_array().expect("a list").is_empty());
    }

    // ── disk conflicts on their blocks (F2.4) ──────────────────────────────

    const X: &str = "content/post/x.md";

    fn x_path() -> editorial_types::ContentPath {
        editorial_types::ContentPath::parse(X).expect("a content path")
    }

    /// The editor has `mine` unsaved, and the file then changes to `disk`
    /// outside Presemble.
    fn conflicted(conductor: &Conductor, tmp: &crate::test_site::TestSite, mine: &str, disk: &str) {
        conductor.handle_command(Command::EditorChanged {
            path: x_path(),
            text: mine.to_string(),
        });
        std::fs::write(tmp.path().join(X), disk).unwrap();
        conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(X).expect("a site path")],
        });
    }

    fn open_editor(conductor: &Conductor, text: &str) {
        conductor.handle_command(Command::EditorOpened {
            path: x_path(),
            text: text.to_string(),
        });
    }

    /// The conflicted regions of the page's document.
    fn regions(conductor: &Conductor) -> Vec<crate::protocol::ConflictRegion> {
        match conductor.handle_command(Command::GetConflicts).response {
            Response::Conflicts(files) => files
                .into_iter()
                .find(|c| c.file == X)
                .map(|c| match c.state {
                    crate::protocol::ConflictState::Blocks(regions) => regions,
                    _ => Vec::new(),
                })
                .unwrap_or_default(),
            other => panic!("expected Conflicts, got {other:?}"),
        }
    }

    /// The conflicts an element carries.
    fn conflicts_on(tag: &str) -> Vec<Value> {
        let name = "data-presemble-conflict=\"";
        let start = tag.find(name).expect("the attribute") + name.len();
        let end = start + tag[start..].find('"').expect("closed");
        serde_json::from_str(&unescape(&tag[start..end])).expect("a JSON array")
    }

    /// The editorial page as served: the file the last build wrote.
    fn served_editorial(conductor: &Conductor) -> String {
        let path = site_index::output_path_for_stem_slug(&conductor.output_dir, "post", "x");
        std::fs::read_to_string(path).expect("the editorial page")
    }

    const BASE: &str = "# Title\n\n----\n\nAlpha.\n\nBeta.\n";

    #[test]
    fn editorial_page_marks_a_conflicted_block_with_its_id() {
        let (tmp, conductor) = conductor_with(BASE);
        open_editor(&conductor, BASE);
        conflicted(
            &conductor,
            &tmp,
            &BASE.replace("Alpha.", "Alpha mine."),
            &BASE.replace("Alpha.", "Alpha disk."),
        );
        let regions = regions(&conductor);
        assert_eq!(regions.len(), 1, "{regions:?}");

        for html in [
            conductor
                .render_page("/post/x", &RenderMode::View)
                .expect("rendered"),
            served_editorial(&conductor),
        ] {
            let tags = tags_with(&html, "data-presemble-conflict");
            assert_eq!(tags.len(), 1, "{tags:#?}");
            assert!(tags[0].starts_with("<p "), "{}", tags[0]);
            assert!(
                tags[0].contains("&quot;paragraph&quot; 0"),
                "the first paragraph: {}",
                tags[0]
            );
            let carried = conflicts_on(&tags[0]);
            assert_eq!(carried.len(), 1, "{carried:#?}");
            assert_eq!(carried[0]["id"], regions[0].id.to_string());
            assert_eq!(carried[0]["file"], X);
            assert_eq!(carried[0]["disk"], "Alpha disk.");
            assert!(
                carried[0]["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("changed on disk")),
                "{carried:#?}"
            );
        }
    }

    #[test]
    fn a_conflict_in_a_list_item_lands_on_its_block() {
        let base = "# Title\n\n----\n\nIntro.\n\n- one\n\n- two\n";
        let (tmp, conductor) = conductor_with(base);
        open_editor(&conductor, base);
        conflicted(
            &conductor,
            &tmp,
            &base.replace("- two", "- two mine"),
            &base.replace("- two", "- two disk"),
        );
        let html = served_editorial(&conductor);
        let tags = tags_with(&html, "data-presemble-conflict");
        assert_eq!(tags.len(), 1, "{tags:#?}");
        assert!(
            tags[0].contains("data-presemble-path=\"i1/b0\""),
            "{}",
            tags[0]
        );
    }

    /// The page as served follows the conflict: resolving its last block
    /// takes the marker off, and announces the page rebuilt.
    #[test]
    fn resolving_a_conflict_rebuilds_its_page_without_the_marker() {
        let (tmp, conductor) = conductor_with(BASE);
        open_editor(&conductor, BASE);
        conflicted(
            &conductor,
            &tmp,
            &BASE.replace("Alpha.", "Alpha mine."),
            &BASE.replace("Alpha.", "Alpha disk."),
        );
        let id = regions(&conductor)[0].id;
        let result = conductor.handle_command(Command::ResolveConflict {
            path: x_path(),
            id,
            take: crate::protocol::ConflictSide::Unsaved,
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert!(
            result.events.iter().any(|e| matches!(
                e,
                ConductorEvent::PagesRebuilt { pages, .. } if pages.contains(&"/post/x".to_string())
            )),
            "{:?}",
            result.events
        );
        assert!(
            tags_with(&served_editorial(&conductor), "data-presemble-conflict").is_empty(),
            "no marker left"
        );
    }

    /// How many times the result announces `page` rebuilt.
    fn times_rebuilt(result: &CommandResult, page: &str) -> usize {
        result
            .events
            .iter()
            .map(|e| match e {
                ConductorEvent::PagesRebuilt { pages, .. } => {
                    pages.iter().filter(|p| p.as_str() == page).count()
                }
                _ => 0,
            })
            .sum()
    }

    /// A file change that brings a conflict renders every page once, after
    /// the conflict is recorded: the page is not rendered (and announced)
    /// again for its conflict.
    #[test]
    fn a_conflict_change_after_a_file_change_renders_the_page_once() {
        let (tmp, conductor) = conductor_with(BASE);
        open_editor(&conductor, BASE);
        conductor.handle_command(Command::EditorChanged {
            path: x_path(),
            text: BASE.replace("Alpha.", "Alpha mine."),
        });
        std::fs::write(tmp.path().join(X), BASE.replace("Alpha.", "Alpha disk.")).unwrap();
        let result = conductor.handle_command(Command::FileChanged {
            paths: vec![editorial_types::SitePath::parse(X).expect("a site path")],
        });
        assert_eq!(regions(&conductor).len(), 1);
        assert_eq!(times_rebuilt(&result, "/post/x"), 1, "{:?}", result.events);
        // ... and that one render has the marker.
        assert_eq!(
            tags_with(&served_editorial(&conductor), "data-presemble-conflict").len(),
            1
        );
    }

    /// GUARD: taking the file's version renders the page while the plan runs,
    /// before the conflict is recorded as ended; the page is rendered again
    /// (once) so it carries no marker.
    #[test]
    fn taking_the_files_version_rebuilds_its_page_without_the_marker() {
        let (tmp, conductor) = conductor_with(BASE);
        open_editor(&conductor, BASE);
        conflicted(
            &conductor,
            &tmp,
            &BASE.replace("Alpha.", "Alpha mine."),
            &BASE.replace("Alpha.", "Alpha disk."),
        );
        let id = regions(&conductor)[0].id;
        let result = conductor.handle_command(Command::ResolveConflict {
            path: x_path(),
            id,
            take: crate::protocol::ConflictSide::Disk,
        });
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        let html = served_editorial(&conductor);
        assert!(html.contains("Alpha disk."), "the file's version is shown");
        assert!(
            tags_with(&html, "data-presemble-conflict").is_empty(),
            "no marker left"
        );
        assert!(
            times_rebuilt(&result, "/post/x") >= 1,
            "{:?}",
            result.events
        );
    }

    /// A conflict whose block N no longer has (the unsaved edit removed it)
    /// has no element to carry it: the page script lists it, so the page can
    /// still offer its resolution.
    #[test]
    fn a_conflict_without_a_block_is_listed_in_the_page_script() {
        let (tmp, conductor) = conductor_with(BASE);
        open_editor(&conductor, BASE);
        conflicted(
            &conductor,
            &tmp,
            &BASE.replace("Alpha.\n\n", ""),
            &BASE.replace("Alpha.", "Alpha disk."),
        );
        let regions = regions(&conductor);
        assert_eq!(regions.len(), 1, "{regions:?}");
        let html = served_editorial(&conductor);
        assert!(tags_with(&html, "data-presemble-conflict").is_empty());
        let script = page_script(&html);
        let listed = script["conflicts"].as_array().expect("a list");
        assert_eq!(listed.len(), 1, "{script}");
        assert_eq!(listed[0]["id"], regions[0].id.to_string());
        assert_eq!(listed[0]["disk"], "Alpha disk.");
    }

    /// GUARD: a page with no conflict carries no marker, and its page script
    /// lists no unplaced conflict.
    #[test]
    fn a_page_without_a_conflict_carries_no_conflict_data() {
        let (_tmp, conductor) = conductor_with(BASE);
        let html = served_editorial(&conductor);
        assert!(tags_with(&html, "data-presemble-conflict").is_empty());
        let script = page_script(&html);
        assert!(
            script["conflicts"].as_array().expect("a list").is_empty(),
            "{script}"
        );
    }

    /// GUARD: published output carries no conflict data, even while one is
    /// recorded.
    #[test]
    fn published_output_carries_no_conflict_data() {
        let (tmp, conductor) = conductor_with(BASE);
        open_editor(&conductor, BASE);
        conflicted(
            &conductor,
            &tmp,
            &BASE.replace("Alpha.", "Alpha mine."),
            &BASE.replace("Alpha.", "Alpha disk."),
        );
        assert_eq!(regions(&conductor).len(), 1);
        conductor.build_all_pages(template::OutputMode::Published);
        let path = site_index::output_path_for_stem_slug(&conductor.output_dir, "post", "x");
        let html = std::fs::read_to_string(path).expect("the page");
        assert!(!html.contains("presemble-conflict"), "{html}");
    }

    /// GUARD: published output carries neither diagnostics nor paths.
    #[test]
    fn published_output_carries_no_diagnostics_and_no_paths() {
        let (_tmp, conductor) = conductor_with(
            "# Title\n\nStray.\n\n----\n\n- fine\n\n- a <b>x</b> b\n\n> q\n>\n> q <i>y</i>\n",
        );
        conductor.build_all_pages(template::OutputMode::Published);
        let path = site_index::output_path_for_stem_slug(&conductor.output_dir, "post", "x");
        let html = std::fs::read_to_string(path).expect("the page");
        for editor_only in [
            "data-presemble-diagnostics",
            "data-presemble-path",
            "presemble-diagnostics",
        ] {
            assert!(!html.contains(editor_only), "{editor_only}: {html}");
        }
    }
}
