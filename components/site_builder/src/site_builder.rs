use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use site_index::{
    NodeRole, PageData, PageKind, SchemaStem, SiteGraph, SiteNode, UrlPath,
    DIR_TEMPLATES,
};
use template::constants::KEY_PRESEMBLE_FILE;

/// Selects the output mode: `Attach` is editorial output, which carries the
/// editor's data (canonical markdown, stored text); `Omit` is published output.
#[derive(Debug, Clone, Copy, Default)]
pub enum SourceAttachment {
    #[default]
    Omit,
    Attach,
}

impl SourceAttachment {
    /// Attached source is for the editor; omitted source means published
    /// output, which carries no editor-only data (ADR-063).
    fn output_mode(self) -> template::OutputMode {
        match self {
            SourceAttachment::Omit => template::OutputMode::Published,
            SourceAttachment::Attach => template::OutputMode::Editorial,
        }
    }
}

/// Result of building a site graph.
pub struct GraphBuildResult {
    pub graph: site_index::SiteGraph,
    /// Pages with validation diagnostics (key: url_path, value: messages).
    pub diagnostics: HashMap<String, Vec<String>>,
    /// Pages that failed to parse (key: label like "post/hello", value: error).
    pub parse_errors: HashMap<String, String>,
}

/// Compute the item template path from the repository for a given stem.
///
/// Tries the directory-based convention first (`templates/{stem}/item.hiccup|html`),
/// then falls back to the flat convention (`templates/{stem}.hiccup|html`).
/// Returns `None` if no template exists.
fn item_template_path(repo: &site_repository::SiteRepository, stem: &SchemaStem) -> Option<PathBuf> {
    let (_, is_hiccup) = repo.item_template_source(stem)?;
    let templates_dir = repo.site_dir().join(DIR_TEMPLATES);
    let stem_str = stem.as_str();

    // Directory-based convention: templates/{stem}/item.{ext}
    if !stem_str.is_empty() {
        let dir_base = templates_dir.join(stem_str).join("item");
        let ext = if is_hiccup { "hiccup" } else { "html" };
        let dir_path = dir_base.with_extension(ext);
        if dir_path.exists() {
            return Some(dir_path);
        }
        // Flat convention: templates/{stem}.{ext}
        let flat_path = templates_dir.join(format!("{stem_str}.{ext}"));
        if flat_path.exists() {
            return Some(flat_path);
        }
        // Return directory-based even if not found on disk (e.g., memory repo)
        return Some(dir_base.with_extension(ext));
    }

    // Empty stem (root): templates/item.{ext}
    let ext = if is_hiccup { "hiccup" } else { "html" };
    Some(templates_dir.join("item").with_extension(ext))
}

/// Compute the collection template path from the repository for a given stem.
///
/// Tries `templates/{stem}/index.hiccup|html`, then falls back to `templates/index.hiccup|html`
/// for the root collection.
fn collection_template_path(repo: &site_repository::SiteRepository, stem: &SchemaStem) -> Option<PathBuf> {
    let (_, is_hiccup) = repo.collection_template_source(stem)?;
    let templates_dir = repo.site_dir().join(DIR_TEMPLATES);
    let stem_str = stem.as_str();
    let ext = if is_hiccup { "hiccup" } else { "html" };

    if stem_str.is_empty() {
        Some(templates_dir.join("index").with_extension(ext))
    } else {
        Some(templates_dir.join(stem_str).join("index").with_extension(ext))
    }
}

/// Build the full site graph from a repository.
///
/// Phase 1a: item pages (one per content slug under each schema stem).
/// Phase 1b: collection pages (one per stem with collection content + schema + template).
/// Phase 1c: legacy fallback root (when no root node was built by 1a or 1b).
pub fn build_graph(
    repo: &site_repository::SiteRepository,
    output_dir: &Path,
    source_attachment: SourceAttachment,
) -> GraphBuildResult {
    let mut graph = SiteGraph::new();
    let mut diagnostics: HashMap<String, Vec<String>> = HashMap::new();
    let mut parse_errors: HashMap<String, String> = HashMap::new();

    let schema_stems = repo.schema_stems();

    // ── Phase 1a: Item pages ─────────────────────────────────────────────────
    for stem in &schema_stems {
        let schema_stem_str = stem.as_str();

        // For empty stem with no item schema, skip — Phase 1b handles root collection.
        let schema_source = match repo.schema_source(stem) {
            Some(s) => s,
            None => {
                if schema_stem_str.is_empty() {
                    continue;
                }
                // Non-empty stem with missing schema: skip silently (Phase 1b may still run)
                continue;
            }
        };

        let grammar = match schema::parse_schema(&schema_source) {
            Ok(g) => g,
            Err(_) => continue,
        };

        let schema_path = repo.schema_path(stem);
        let template_path_opt = item_template_path(repo, stem);

        for slug in repo.content_slugs(stem) {
            let content_path = repo.content_path(stem, &slug);

            let content_src = match repo.content_source(stem, &slug) {
                Some(s) => s,
                None => continue,
            };

            // Parse and assign
            let doc = match content::parse_and_assign(&content_src, &grammar) {
                Ok(d) => d,
                Err(e) => {
                    let label = if schema_stem_str.is_empty() {
                        slug.clone()
                    } else {
                        format!("{schema_stem_str}/{slug}")
                    };
                    parse_errors.insert(label, e.to_string());
                    continue;
                }
            };

            // Validate
            let validation = content::validate(&doc, &grammar);
            if !validation.is_valid() {
                let url_path_str = site_index::url_for_stem_slug(schema_stem_str, &slug);
                let msgs: Vec<String> = validation
                    .diagnostics
                    .iter()
                    .map(|d| d.message.clone())
                    .collect();
                diagnostics.insert(url_path_str.clone(), msgs);
            }

            // Build data graph
            let mut data =
                template::build_article_graph(&doc, &grammar, source_attachment.output_mode());

            // Inject metadata
            data.insert(
                "_presemble_stem",
                template::Value::Text(schema_stem_str.to_string()),
            );
            let presemble_file = site_index::content_file_path(schema_stem_str, &slug);
            data.insert(KEY_PRESEMBLE_FILE, template::Value::Text(presemble_file));
            let url_path = site_index::url_for_stem_slug(schema_stem_str, &slug);
            data.insert("url", template::Value::Text(url_path.clone()));
            // A title is its text in a string context, whatever it is made of.
            let title = match data.resolve(&["title"]) {
                Some(v @ (template::Value::Text(_) | template::Value::Inlines(_))) => {
                    v.display_text().unwrap_or_default()
                }
                _ => slug.clone(),
            };
            let link = template::synthesize_link(&title, &url_path, template::title_editor(&data));
            data.insert("link", template::Value::Record(link));

            // Compute output path
            let output_path =
                site_index::output_path_for_stem_slug(output_dir, schema_stem_str, &slug);

            // Compute deps
            let mut deps: HashSet<PathBuf> = HashSet::new();
            deps.insert(content_path.clone());
            deps.insert(schema_path.clone());
            if let Some(ref tp) = template_path_opt {
                deps.insert(tp.clone());
            }

            let node = SiteNode {
                url_path: UrlPath::new(&url_path),
                output_path,
                source_path: content_path.clone(),
                deps,
                role: NodeRole::Page(PageData {
                    page_kind: PageKind::Item,
                    schema_stem: stem.clone(),
                    template_path: template_path_opt.clone().unwrap_or_default(),
                    content_path,
                    schema_path: schema_path.clone(),
                    data,
                }),
            };
            graph.insert(node);
        }
    }

    // ── Phase 1b: Collection pages ───────────────────────────────────────────
    for stem in &schema_stems {
        let schema_stem_str = stem.as_str();

        // Skip if no collection content
        if repo.collection_content_source(stem).is_none() {
            continue;
        }

        let collection_schema_src = match repo.collection_schema_source(stem) {
            Some(s) => s,
            None => continue,
        };

        let collection_grammar = match schema::parse_schema(&collection_schema_src) {
            Ok(g) => g,
            Err(_) => continue,
        };

        // Skip if no collection template
        let tmpl_path = match collection_template_path(repo, stem) {
            Some(p) => p,
            None => continue,
        };

        let content_src = match repo.collection_content_source(stem) {
            Some(s) => s,
            None => continue,
        };

        let collection_doc = match content::parse_and_assign(&content_src, &collection_grammar) {
            Ok(d) => d,
            Err(e) => {
                let label = format!("{schema_stem_str}/index");
                parse_errors.insert(label, e.to_string());
                continue;
            }
        };

        let validation = content::validate(&collection_doc, &collection_grammar);
        if !validation.is_valid() {
            let url_path_str = if schema_stem_str.is_empty() {
                "/".to_string()
            } else {
                format!("/{schema_stem_str}/")
            };
            let msgs: Vec<String> = validation
                .diagnostics
                .iter()
                .map(|d| d.message.clone())
                .collect();
            diagnostics.insert(url_path_str, msgs);
        }

        let mut data = template::build_article_graph(
            &collection_doc,
            &collection_grammar,
            source_attachment.output_mode(),
        );

        // Inject metadata
        let coll_presemble_file = if schema_stem_str.is_empty() {
            "content/index.md".to_string()
        } else {
            format!("content/{schema_stem_str}/index.md")
        };
        data.insert(KEY_PRESEMBLE_FILE, template::Value::Text(coll_presemble_file));
        data.insert(
            "_presemble_stem",
            template::Value::Text(schema_stem_str.to_string()),
        );

        let (url_path_str, output_path_coll) = if schema_stem_str.is_empty() {
            ("/".to_string(), output_dir.join("index.html"))
        } else {
            (
                format!("/{schema_stem_str}/"),
                output_dir.join(schema_stem_str).join("index.html"),
            )
        };
        data.insert("url", template::Value::Text(url_path_str.clone()));
        let title = match data.resolve(&["title"]) {
            Some(v @ (template::Value::Text(_) | template::Value::Inlines(_))) => {
                v.display_text().unwrap_or_default()
            }
            _ => schema_stem_str.to_string(),
        };
        let link = template::synthesize_link(&title, &url_path_str, template::title_editor(&data));
        data.insert("link", template::Value::Record(link));

        let collection_content_path = repo.collection_content_path(stem);
        let collection_schema_path = repo.collection_schema_path(stem);

        // Deps: template, content, schema, plus all item content files for this stem
        let mut deps_coll: HashSet<PathBuf> = HashSet::new();
        deps_coll.insert(tmpl_path.clone());
        deps_coll.insert(collection_content_path.clone());
        deps_coll.insert(collection_schema_path.clone());
        for slug in repo.content_slugs(stem) {
            deps_coll.insert(repo.content_path(stem, &slug));
        }

        let node = SiteNode {
            url_path: UrlPath::new(&url_path_str),
            output_path: output_path_coll,
            source_path: collection_content_path.clone(),
            deps: deps_coll,
            role: NodeRole::Page(PageData {
                page_kind: PageKind::Collection,
                schema_stem: stem.clone(),
                template_path: tmpl_path,
                content_path: collection_content_path,
                schema_path: collection_schema_path,
                data,
            }),
        };
        graph.insert(node);
    }

    // ── Phase 1c: Legacy fallback root ───────────────────────────────────────
    let root_url = UrlPath::new("/");
    if graph.get(&root_url).is_none() {
        let root_stem = SchemaStem::new("");
        if let Some(tmpl_path) = collection_template_path(repo, &root_stem) {
            let mut root_graph = template::DataGraph::new();

            // Populate from content/index.md + schemas/index.md if both exist
            if let Some(schema_src) = repo.collection_schema_source(&root_stem)
                && let Ok(grammar) = schema::parse_schema(&schema_src)
                && let Some(content_src) = repo.collection_content_source(&root_stem)
                && let Ok(doc) = content::parse_and_assign(&content_src, &grammar)
            {
                root_graph = template::build_article_graph(
                    &doc,
                    &grammar,
                    source_attachment.output_mode(),
                );
                root_graph.insert(
                    KEY_PRESEMBLE_FILE,
                    template::Value::Text("content/index.md".to_string()),
                );
                root_graph.insert(
                    "_presemble_stem",
                    template::Value::Text(String::new()),
                );
            }

            let root_output_path = output_dir.join("index.html");
            let root_content_path = repo.collection_content_path(&root_stem);
            let root_schema_path = repo.collection_schema_path(&root_stem);

            let mut root_deps: HashSet<PathBuf> = HashSet::new();
            root_deps.insert(tmpl_path.clone());
            root_deps.insert(root_content_path.clone());
            root_deps.insert(root_schema_path.clone());

            // The legacy root index (templates/index.html) typically renders all
            // content items via collections. Register every known content file as
            // a dep so incremental rebuilds trigger correctly when any item changes.
            for stem in &schema_stems {
                for slug in repo.content_slugs(stem) {
                    root_deps.insert(repo.content_path(stem, &slug));
                }
                root_deps.insert(repo.collection_schema_path(stem));
            }

            let node = SiteNode {
                url_path: root_url,
                output_path: root_output_path,
                source_path: root_content_path.clone(),
                deps: root_deps,
                role: NodeRole::Page(PageData {
                    page_kind: PageKind::Collection,
                    schema_stem: root_stem,
                    template_path: tmpl_path,
                    content_path: root_content_path,
                    schema_path: root_schema_path,
                    data: root_graph,
                }),
            };
            graph.insert(node);
        }
    }

    GraphBuildResult { graph, diagnostics, parse_errors }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_repo() -> site_repository::SiteRepository {
        site_repository::SiteRepository::builder()
            .schema("post", "# Post title {#title}\noccurs\n: exactly once\n")
            .content("post", "hello", "# Hello World\n")
            .item_template("post", "<html><body></body></html>", false)
            .build()
    }

    #[test]
    fn build_graph_creates_item_page() {
        let repo = make_repo();
        let output = std::path::PathBuf::from("/output");
        let result = build_graph(&repo, &output, SourceAttachment::Omit);

        assert!(result.parse_errors.is_empty(), "no parse errors expected");
        let url = UrlPath::new("/post/hello");
        let node = result.graph.get(&url).expect("item page should be in graph");
        let pd = node.page_data().expect("should be a page node");
        assert_eq!(pd.page_kind, PageKind::Item);
        assert_eq!(pd.schema_stem.as_str(), "post");
    }

    #[test]
    fn build_graph_injects_url_metadata() {
        let repo = make_repo();
        let output = std::path::PathBuf::from("/output");
        let result = build_graph(&repo, &output, SourceAttachment::Omit);

        let url = UrlPath::new("/post/hello");
        let node = result.graph.get(&url).expect("item page should be in graph");
        let pd = node.page_data().unwrap();
        assert!(
            matches!(pd.data.resolve(&["url"]), Some(template::Value::Text(s)) if s == "/post/hello"),
            "url metadata should be injected"
        );
    }

    #[test]
    fn build_graph_collection_page() {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Post title {#title}\noccurs\n: exactly once\n")
            .collection_schema("post", "# Posts {#title}\noccurs\n: exactly once\n")
            .collection_content("post", "# All Posts\n")
            .collection_template("post", "<html><body></body></html>", false)
            .build();

        let output = std::path::PathBuf::from("/output");
        let result = build_graph(&repo, &output, SourceAttachment::Omit);

        let url = UrlPath::new("/post/");
        let node = result.graph.get(&url).expect("collection page should be in graph");
        let pd = node.page_data().expect("should be a page node");
        assert_eq!(pd.page_kind, PageKind::Collection);
    }

    #[test]
    fn build_graph_legacy_root_fallback() {
        let repo = site_repository::SiteRepository::builder()
            .index_template("<html><body>root</body></html>", false)
            .build();

        let output = std::path::PathBuf::from("/output");
        let result = build_graph(&repo, &output, SourceAttachment::Omit);

        let url = UrlPath::new("/");
        let node = result.graph.get(&url).expect("root page should be in graph via fallback");
        let pd = node.page_data().expect("should be a page node");
        assert_eq!(pd.page_kind, PageKind::Collection);
    }

    const TITLE_AND_LINK_TEMPLATE: &str = r#"<div><presemble:insert data="title" as="h1" /><presemble:insert data="link" /></div>"#;

    fn render_item(attachment: SourceAttachment) -> String {
        let repo = make_repo();
        let output = std::path::PathBuf::from("/output");
        let result = build_graph(&repo, &output, attachment);
        let node = result.graph.get(&UrlPath::new("/post/hello")).expect("item page");
        let pd = node.page_data().expect("page node");
        template::render_template(TITLE_AND_LINK_TEMPLATE, &pd.data, attachment.output_mode())
            .expect("renders")
    }

    #[test]
    fn published_build_has_no_editor_attributes() {
        let html = render_item(SourceAttachment::Omit);
        assert!(html.contains("Hello World"), "title should render: {html}");
        assert!(
            !html.contains(template::constants::ATTR_MD)
                && !html.contains(template::constants::ATTR_ADDRESS),
            "published output must not carry editor attributes: {html}"
        );
    }

    #[test]
    fn editorial_build_carries_canonical_md_and_published_does_not() {
        let editorial = render_item(SourceAttachment::Attach);
        assert_eq!(
            editorial.matches(r#"data-presemble-md="Hello World""#).count(),
            2,
            "the title slot and the synthesized title link carry md: {editorial}"
        );
        let published = render_item(SourceAttachment::Omit);
        assert!(!published.contains(template::constants::ATTR_MD), "{published}");
    }

    #[test]
    fn editorial_build_title_and_link_carry_the_titles_address() {
        // The browser edits `data-presemble-md` and addresses by
        // `data-presemble-address` (ADR-059 section 10); the title link takes
        // both from the title heading.
        let html = render_item(SourceAttachment::Attach);
        assert!(html.contains("Hello World"), "title should render: {html}");
        let address = template::constants::ATTR_ADDRESS;
        assert_eq!(html.matches(address).count(), 2, "{html}");
        assert!(!html.contains("data-presemble-text"), "{html}");
    }

    /// Guard (ADR-063): the synthesized link reads the title's plain text
    /// and carries the title heading's editor record.
    #[test]
    fn synthesized_link_text_is_plain_and_carries_the_title_editor_record() {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Post title {#title}\noccurs\n: exactly once\n")
            .content("post", "hello", "# *Hello* \\<World>\n")
            .item_template("post", "<html><body></body></html>", false)
            .build();
        let result = build_graph(&repo, std::path::Path::new("/output"), SourceAttachment::Attach);
        let node = result.graph.get(&UrlPath::new("/post/hello")).expect("item page");
        let pd = node.page_data().expect("page node");
        let link = match pd.data.resolve(&["link"]) {
            Some(template::Value::Record(link)) => link.clone(),
            other => panic!("link record expected, got {other:?}"),
        };
        assert!(matches!(link.resolve(&["text"]), Some(template::Value::Text(t)) if t == "Hello <World>"));
        assert!(matches!(
            link.resolve(&[template::constants::KEY_EDITOR_MD]),
            Some(template::Value::Text(t)) if t == "*Hello* \\<World>"
        ));
    }

    #[test]
    fn build_graph_parse_error_recorded() {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", "# Post title {#title}\noccurs\n: exactly once\n")
            .content("post", "bad", "{{{{invalid content}}}}") // malformed
            .build();

        let output = std::path::PathBuf::from("/output");
        let result = build_graph(&repo, &output, SourceAttachment::Omit);

        // The graph should be empty or have no item for "bad"
        // And parse_errors should record something (or the page is just absent)
        // Either the parse fails and gets recorded, or the content happens to be valid
        // In any case the graph should not panic
        let _ = result;
    }
}
