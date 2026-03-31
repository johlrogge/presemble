use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

// ── Helpers ─────────────────────────────────────────────────────────────────

fn fixture_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/blog-site")
}

/// Copies the blog-site fixture into a fresh temp directory, excluding output/.
/// Returns (TempDir, PathBuf to site root) — caller must hold TempDir to keep it alive.
fn copy_fixture_site() -> (TempDir, PathBuf) {
    let src = fixture_src();
    let tmp = TempDir::new().expect("failed to create temp dir");
    let dest = tmp.path().join("blog-site");
    copy_dir_excluding(&src, &dest, "output");
    (tmp, dest)
}

fn copy_dir_excluding(src: &Path, dest: &Path, exclude: &str) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap().flatten() {
        let name = entry.file_name();
        if name == exclude {
            continue;
        }
        let dest_path = dest.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir_excluding(&entry.path(), &dest_path, exclude);
        } else {
            fs::copy(entry.path(), &dest_path).unwrap();
        }
    }
}

// ── Read-only validation tests (never call build_site) ────────────────────

#[test]
fn fixture_blog_site_has_required_inputs() {
    let site = fixture_src();
    assert!(site.join("schemas/article.md").exists());
    assert!(site.join("templates/article.html").exists());
    assert!(site.join("content/article/hello-world.md").exists());
}

#[test]
fn hello_world_is_valid_against_article_schema() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article.md");
    let content_src =
        include_str!("../../../fixtures/blog-site/content/article/hello-world.md");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    let doc = content::parse_document(content_src).expect("content parses");
    let result = content::validate(&doc, &grammar);

    assert!(result.is_valid(), "hello-world should be valid: {result:?}");
}

#[test]
fn invalid_post_fails_validation_with_title_and_body_errors() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article.md");
    let content_src =
        include_str!("../../../fixtures/blog-site/content/article/invalid-post.md");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    let doc = content::parse_document(content_src).expect("content parses");
    let result = content::validate(&doc, &grammar);

    assert!(!result.is_valid(), "invalid-post should fail validation");
    let messages: Vec<_> = result.diagnostics.iter().map(|d| &d.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("title")),
        "should have title error; messages: {messages:?}"
    );
}

// ── Build tests (each gets its own temp dir) ──────────────────────────────

#[test]
fn build_produces_index_html() {
    let (_tmp, site_dir) = copy_fixture_site();

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    let index_path = publisher_cli::output_dir(&site_dir).join("index.html");
    assert!(index_path.exists(), "output/index.html should be created");

    let content = fs::read_to_string(&index_path).unwrap();
    assert!(
        content.contains("article/hello-world"),
        "index should link to hello-world article: {content}"
    );
    assert!(
        content.contains("Hello, World"),
        "index should contain article title: {content}"
    );
    assert!(
        outcome.built_pages.contains_key("article"),
        "article pages should be collected"
    );
}

#[test]
fn build_site_populates_dep_graph_for_article() {
    let (_tmp, site_dir) = copy_fixture_site();
    let site_dir = fs::canonicalize(&site_dir).unwrap();
    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    let article_output = publisher_cli::output_dir(&site_dir).join("article/hello-world/index.html");
    let schema_path = site_dir.join("schemas/article.md");
    let content_path = site_dir.join("content/article/hello-world.md");

    let affected = outcome.dep_graph.affected_outputs(&schema_path);
    assert!(
        affected.contains(&article_output),
        "schema change should affect article output; affected: {affected:?}"
    );

    let affected = outcome.dep_graph.affected_outputs(&content_path);
    assert!(
        affected.contains(&article_output),
        "content change should affect article output"
    );
}

#[test]
fn build_site_dep_graph_index_depends_on_all_content() {
    let (_tmp, site_dir) = copy_fixture_site();
    let site_dir = fs::canonicalize(&site_dir).unwrap();
    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    let index_output = publisher_cli::output_dir(&site_dir).join("index.html");
    let content_path = site_dir.join("content/article/hello-world.md");

    let affected = outcome.dep_graph.affected_outputs(&content_path);
    assert!(
        affected.contains(&index_output),
        "content change should affect index.html; affected: {affected:?}"
    );
}

#[test]
fn build_site_dep_graph_index_depends_on_index_template() {
    let (_tmp, site_dir) = copy_fixture_site();
    let site_dir = fs::canonicalize(&site_dir).unwrap();
    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    let index_output = publisher_cli::output_dir(&site_dir).join("index.html");
    let index_template = site_dir.join("templates/index.html");

    let affected = outcome.dep_graph.affected_outputs(&index_template);
    assert!(
        affected.contains(&index_output),
        "template change should affect index; affected: {affected:?}"
    );
}

#[test]
fn build_site_articles_collection_has_url_field() {
    let (_tmp, site_dir) = copy_fixture_site();

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    let articles = outcome
        .built_pages
        .get("article")
        .expect("article pages should exist");
    assert!(!articles.is_empty(), "should have at least one article");

    let article = &articles[0];
    assert!(!article.url_path.is_empty(), "url_path should be set");
    assert!(
        article.url_path.starts_with("/article/"),
        "url_path should start with /article/: {}",
        article.url_path
    );

    match article.data.resolve(&["url"]) {
        Some(template::Value::Text(url)) => {
            assert!(
                url.starts_with("/article/"),
                "url field should be a path: {url}"
            );
        }
        other => panic!("expected url Text field, got: {other:?}"),
    }
}

#[test]
fn build_site_copies_assets_to_output() {
    let (_tmp, site_dir) = copy_fixture_site();

    publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    let asset = publisher_cli::output_dir(&site_dir).join("assets/style.css");
    assert!(
        asset.exists(),
        "output/assets/style.css should be copied from assets/style.css"
    );
}

#[test]
fn presemble_include_inlines_header_and_footer_fragments() {
    // Build a minimal site where the index template uses presemble:include
    // for header and footer fragments, and verify the output HTML contains
    // the fragment content rather than the include directive.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("include-site");

    // Create directory structure
    fs::create_dir_all(site.join("schemas")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    // Schema
    fs::write(
        site.join("schemas/article.md"),
        "# Article title {#title}\noccurs\n: exactly once\n",
    )
    .unwrap();

    // Content
    fs::write(
        site.join("content/article/test-post.md"),
        "# Test Post\n",
    )
    .unwrap();

    // Fragment templates
    fs::write(
        site.join("templates/header.html"),
        r#"<header class="site-header"><a href="/">MySite</a></header>"#,
    )
    .unwrap();
    fs::write(
        site.join("templates/footer.html"),
        r#"<footer class="site-footer"><p>Footer text</p></footer>"#,
    )
    .unwrap();

    // Article template using presemble:include
    fs::write(
        site.join("templates/article.html"),
        r#"<!DOCTYPE html>
<html lang="en">
<head><title>Test</title></head>
<body>
<presemble:include src="header" />
<main><presemble:insert data="article.title" as="h1" /></main>
<presemble:include src="footer" />
</body>
</html>"#,
    )
    .unwrap();

    // Index template using presemble:include
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html>
<html lang="en">
<head><title>Index</title></head>
<body>
<presemble:include src="header" />
<main>Home</main>
<presemble:include src="footer" />
</body>
</html>"#,
    )
    .unwrap();

    // Minimal CSS asset so asset copy doesn't fail
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome = publisher_cli::build_site(&site, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");
    assert_eq!(outcome.files_failed, 0, "no pages should fail");

    // Verify article output contains header and footer content
    let article_html =
        fs::read_to_string(publisher_cli::output_dir(&site).join("article/test-post/index.html")).unwrap();
    assert!(
        article_html.contains("MySite"),
        "article output should contain header content from include: {article_html}"
    );
    assert!(
        article_html.contains("Footer text"),
        "article output should contain footer content from include: {article_html}"
    );
    assert!(
        !article_html.contains("presemble:include"),
        "article output should not contain presemble:include directive: {article_html}"
    );

    // Verify index output also inlines the fragments
    let index_html = fs::read_to_string(publisher_cli::output_dir(&site).join("index.html")).unwrap();
    assert!(
        index_html.contains("MySite"),
        "index output should contain header content from include: {index_html}"
    );
    assert!(
        index_html.contains("Footer text"),
        "index output should contain footer content from include: {index_html}"
    );
}

#[test]
fn cross_content_reference_resolves_author_data() {
    let (_tmp, site_dir) = copy_fixture_site();
    let site_dir = std::fs::canonicalize(&site_dir).unwrap();

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve).expect("build should succeed");

    // The post should have its author resolved with data from the author page
    let posts = outcome.built_pages.get("article").expect("article pages exist");
    assert!(!posts.is_empty(), "at least one article should exist");

    let post = &posts[0];

    // author.href should still be the original link href
    match post.data.resolve(&["author", "href"]) {
        Some(template::Value::Text(href)) => {
            assert!(href.starts_with("/author/"), "author href should be a path: {href}");
        }
        other => panic!("expected author.href Text, got: {other:?}"),
    }

    // author.name should be resolved from the author page
    match post.data.resolve(&["author", "name"]) {
        Some(template::Value::Text(name)) => {
            assert!(!name.is_empty(), "author name should be non-empty after resolution");
        }
        other => panic!("expected author.name Text after resolution, got: {other:?}"),
    }
}

#[test]
fn invalid_post_is_rendered_with_suggestions_not_skipped() {
    let (_tmp, site_dir) = copy_fixture_site();

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve)
        .expect("build should succeed");

    // The invalid-post should appear in built_pages (rendered with suggestion nodes)
    let articles = outcome
        .built_pages
        .get("article")
        .expect("article pages should exist");
    let invalid_page = articles
        .iter()
        .find(|p| p.url_path.contains("invalid-post"));
    assert!(
        invalid_page.is_some(),
        "invalid-post should be in built_pages (rendered with suggestions), got url_paths: {:?}",
        articles.iter().map(|p| &p.url_path).collect::<Vec<_>>()
    );

    // It should NOT be in build_errors (those are parse failures only)
    let in_errors = outcome
        .build_errors
        .keys()
        .any(|k| k.contains("invalid-post"));
    assert!(
        !in_errors,
        "invalid-post should not be in build_errors (it renders with suggestions, not fail)"
    );

    // It SHOULD be in page_suggestions (validation diagnostics were recorded)
    let in_suggestions = outcome
        .page_suggestions
        .keys()
        .any(|k| k.contains("invalid-post"));
    assert!(
        in_suggestions,
        "invalid-post should be in page_suggestions; keys: {:?}",
        outcome.page_suggestions.keys().collect::<Vec<_>>()
    );

    // files_failed should not count the invalid post
    assert_eq!(
        outcome.files_failed, 0,
        "no pages should be counted as failed (invalid-post renders with suggestions)"
    );

    // files_with_suggestions should be at least 1
    assert!(
        outcome.files_with_suggestions >= 1,
        "at least one page should be counted as having suggestions"
    );

    // The HTML output file should exist (the page was rendered)
    let invalid_post_output = publisher_cli::output_dir(&site_dir)
        .join("article/invalid-post/index.html");
    assert!(
        invalid_post_output.exists(),
        "invalid-post output HTML should exist at {}",
        invalid_post_output.display()
    );
}

#[test]
fn index_content_is_rendered_into_index_page() {
    // Build a site that has schemas/index.md and content/index/index.md
    // and verify that index.* data paths are available in the rendered index.html.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("index-content-site");

    fs::create_dir_all(site.join("schemas")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("content/index")).unwrap();
    fs::create_dir_all(site.join("templates")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    // Article schema and content (required for a valid site)
    fs::write(
        site.join("schemas/article.md"),
        "# Article title {#title}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(
        site.join("content/article/sample.md"),
        "# Sample Article\n",
    )
    .unwrap();

    // Index schema: a page with a site title and tagline
    fs::write(
        site.join("schemas/index.md"),
        "# Site title {#site_title}\noccurs\n: exactly once\n\nTagline for the site. {#tagline}\noccurs\n: exactly once\n",
    )
    .unwrap();

    // Index content
    fs::write(
        site.join("content/index/index.md"),
        "# My Awesome Site\n\nBuilt with Presemble.\n",
    )
    .unwrap();

    // Article template
    fs::write(
        site.join("templates/article.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="article.title" as="h1" /></body></html>"#,
    )
    .unwrap();

    // Index template that uses index.* data paths
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html>
<html>
<head><title><presemble:insert data="index.site_title" /></title></head>
<body>
<h1><presemble:insert data="index.site_title" /></h1>
<p><presemble:insert data="index.tagline" /></p>
</body>
</html>"#,
    )
    .unwrap();

    // Minimal CSS so asset copy doesn't complain
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome =
        publisher_cli::build_site(&site, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve)
            .expect("build should succeed");
    assert_eq!(outcome.files_failed, 0, "no pages should fail");

    let index_html =
        fs::read_to_string(publisher_cli::output_dir(&site).join("index.html")).unwrap();

    assert!(
        index_html.contains("My Awesome Site"),
        "index.html should contain index.site_title from content: {index_html}"
    );
    assert!(
        index_html.contains("Built with Presemble"),
        "index.html should contain index.tagline from content: {index_html}"
    );
}

#[test]
fn index_content_schema_and_content_tracked_as_deps() {
    // When schemas/index.md and content/index/index.md exist, changes to either
    // should trigger a rebuild of index.html.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("index-dep-site");

    fs::create_dir_all(site.join("schemas")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("content/index")).unwrap();
    fs::create_dir_all(site.join("templates")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    fs::write(
        site.join("schemas/article.md"),
        "# Article title {#title}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(
        site.join("content/article/sample.md"),
        "# Sample Article\n",
    )
    .unwrap();

    fs::write(
        site.join("schemas/index.md"),
        "# Site title {#site_title}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(
        site.join("content/index/index.md"),
        "# My Site\n",
    )
    .unwrap();

    fs::write(
        site.join("templates/article.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="article.title" as="h1" /></body></html>"#,
    )
    .unwrap();
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="index.site_title" as="h1" /></body></html>"#,
    )
    .unwrap();
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let site = fs::canonicalize(&site).unwrap();
    let outcome =
        publisher_cli::build_site(&site, &publisher_cli::UrlConfig::default(), publisher_cli::BuildMode::Serve)
            .expect("build should succeed");

    let index_output = publisher_cli::output_dir(&site).join("index.html");
    let index_schema = site.join("schemas/index.md");
    let index_content = site.join("content/index/index.md");

    let affected_by_schema = outcome.dep_graph.affected_outputs(&index_schema);
    assert!(
        affected_by_schema.contains(&index_output),
        "schemas/index.md change should affect index.html; affected: {affected_by_schema:?}"
    );

    let affected_by_content = outcome.dep_graph.affected_outputs(&index_content);
    assert!(
        affected_by_content.contains(&index_output),
        "content/index/index.md change should affect index.html; affected: {affected_by_content:?}"
    );
}

#[test]
fn broken_link_reference_fails_build() {
    // Create a minimal site where an article references a nonexistent author page.
    // In BuildMode::Build the broken reference should count as a build failure.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("broken-ref-site");

    fs::create_dir_all(site.join("schemas")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    // Note: no content/author directory — the author page does NOT exist
    fs::create_dir_all(site.join("templates")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    // Article schema: title + author link
    fs::write(
        site.join("schemas/article.md"),
        "# Article title {#title}\noccurs\n: exactly once\n\n[<name>](/author/<name>) {#author}\noccurs\n: exactly once\n",
    )
    .unwrap();

    // Article content linking to a nonexistent author
    fs::write(
        site.join("content/article/my-post.md"),
        "# My Post\n\n[Ghost Writer](/author/ghost-writer)\n",
    )
    .unwrap();

    // Minimal templates
    fs::write(
        site.join("templates/article.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="article.title" as="h1" /></body></html>"#,
    )
    .unwrap();
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html><html><body><h1>Index</h1></body></html>"#,
    )
    .unwrap();
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome = publisher_cli::build_site(
        &site,
        &publisher_cli::UrlConfig::default(),
        publisher_cli::BuildMode::Build,
    )
    .expect("build_site should not return Err");

    assert!(
        outcome.files_failed > 0,
        "broken content reference should count as a build failure; outcome: files_failed={}, build_errors={:?}",
        outcome.files_failed,
        outcome.build_errors
    );

    // The broken link error should appear in build_errors
    let all_errors: Vec<_> = outcome.build_errors.values().flatten().collect();
    assert!(
        all_errors.iter().any(|msg| msg.contains("ghost-writer") || msg.contains("broken link")),
        "build_errors should mention the broken reference; errors: {all_errors:?}"
    );
}

#[test]
fn broken_link_reference_is_warning_in_serve_mode() {
    // Same setup but BuildMode::Serve — broken references should be warnings
    // (page_suggestions), not hard failures.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("broken-ref-serve-site");

    fs::create_dir_all(site.join("schemas")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    fs::write(
        site.join("schemas/article.md"),
        "# Article title {#title}\noccurs\n: exactly once\n\n[<name>](/author/<name>) {#author}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(
        site.join("content/article/my-post.md"),
        "# My Post\n\n[Ghost Writer](/author/ghost-writer)\n",
    )
    .unwrap();
    fs::write(
        site.join("templates/article.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="article.title" as="h1" /></body></html>"#,
    )
    .unwrap();
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html><html><body><h1>Index</h1></body></html>"#,
    )
    .unwrap();
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome = publisher_cli::build_site(
        &site,
        &publisher_cli::UrlConfig::default(),
        publisher_cli::BuildMode::Serve,
    )
    .expect("build_site should not return Err");

    // In Serve mode broken references are warnings — files_failed should be 0
    assert_eq!(
        outcome.files_failed, 0,
        "broken reference in Serve mode should not count as hard failure"
    );

    // The broken link warning should appear in page_suggestions
    let all_suggestions: Vec<_> = outcome.page_suggestions.values().flatten().collect();
    assert!(
        all_suggestions.iter().any(|msg| msg.contains("ghost-writer") || msg.contains("broken link")),
        "page_suggestions should mention the broken reference in Serve mode; suggestions: {all_suggestions:?}"
    );
}
