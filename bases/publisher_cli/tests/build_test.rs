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

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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
    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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
    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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
    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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

    publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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

    let outcome = publisher_cli::build_site(&site, &publisher_cli::UrlConfig::default()).expect("build should succeed");
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

    let outcome = publisher_cli::build_site(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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
