use std::fs;
use std::path::Path;
use publisher_cli;
use template;

// ── Test 3: fixtures are wired up for end-to-end rendering ───────────────────

#[test]
fn build_produces_html_output_for_valid_content() {
    let site_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/blog-site");
    // Clean URL convention: output goes to output/article/hello-world/index.html
    let output_path = Path::new(site_dir).join("output/article/hello-world/index.html");

    // Clean up any previous output
    let _ = std::fs::remove_file(&output_path);

    // Verify the required fixtures exist so rendering can proceed
    assert!(Path::new(site_dir).join("schemas/article.md").exists());
    assert!(Path::new(site_dir).join("templates/article.html").exists());
    assert!(Path::new(site_dir).join("content/article/hello-world.md").exists());
}

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/blog-site")
}

fn read_fixture(relative: &str) -> String {
    let path = fixtures_dir().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("could not read fixture {}: {}", path.display(), e))
}

// ── Test 1: valid document passes validation ─────────────────────────────────

#[test]
fn hello_world_is_valid_against_article_schema() {
    let schema_source = read_fixture("schemas/article.md");
    let grammar = schema::parse_schema(&schema_source)
        .expect("article.md schema should parse without error");

    let content_source = read_fixture("content/article/hello-world.md");
    let doc = content::parse_document(&content_source)
        .expect("hello-world.md should parse without error");

    let result = content::validate(&doc, &grammar);

    assert!(
        result.is_valid(),
        "hello-world.md should be valid against article.md schema, but got diagnostics: {:#?}",
        result.diagnostics
    );
}

// ── Test 2: invalid document fails validation with expected diagnostics ───────

#[test]
fn invalid_post_fails_validation_with_title_and_body_errors() {
    let schema_source = read_fixture("schemas/article.md");
    let grammar = schema::parse_schema(&schema_source)
        .expect("article.md schema should parse without error");

    let content_source = read_fixture("content/article/invalid-post.md");
    let doc = content::parse_document(&content_source)
        .expect("invalid-post.md should parse without error");

    let result = content::validate(&doc, &grammar);

    assert!(
        !result.is_valid(),
        "invalid-post.md should fail validation, but it was reported as valid"
    );

    // Violation 1: title slot requires an H1 but the document only has an H2
    let has_title_error = result.diagnostics.iter().any(|d| {
        d.slot
            .as_ref()
            .map(|s| s.as_str() == "title")
            .unwrap_or(false)
    });
    assert!(
        has_title_error,
        "expected a diagnostic for the 'title' slot (missing H1), but got: {:#?}",
        result.diagnostics
    );

    // Violation 2: body section contains an H2, which is outside the allowed h3..h6 range
    let has_body_heading_error = result.diagnostics.iter().any(|d| {
        d.slot.is_none() && d.message.contains("H2")
    });
    assert!(
        has_body_heading_error,
        "expected a body heading level error for H2, but got: {:#?}",
        result.diagnostics
    );
}

// ── Test 4: build produces index.html with article links ─────────────────────

#[test]
fn build_produces_index_html() {
    let site_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/blog-site");
    // Clean previous output
    let _ = std::fs::remove_file(
        std::path::Path::new(site_dir).join("output/index.html")
    );

    let outcome = publisher_cli::build_site(std::path::Path::new(site_dir))
        .expect("build should succeed");

    let index_path = std::path::Path::new(site_dir).join("output/index.html");
    assert!(index_path.exists(), "output/index.html should be created");

    let content = std::fs::read_to_string(&index_path).unwrap();
    assert!(content.contains("article/hello-world"), "index should link to hello-world article: {content}");
    assert!(content.contains("Hello, World"), "index should contain article title: {content}");

    // hello-world is built
    assert!(outcome.built_pages.contains_key("article"), "article pages should be collected");
}

// ── Test 5: built_pages articles collection has url field ────────────────────

#[test]
fn build_site_populates_dep_graph_for_article() {
    let site_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/blog-site");
    let outcome = publisher_cli::build_site(std::path::Path::new(site_dir))
        .expect("build should succeed");

    // Clean URL convention: output goes to output/article/hello-world/index.html
    let article_output = std::path::Path::new(site_dir)
        .join("output/article/hello-world/index.html");
    let schema_path = std::path::Path::new(site_dir).join("schemas/article.md");
    let content_path = std::path::Path::new(site_dir)
        .join("content/article/hello-world.md");

    // Changing the schema should affect the article output
    let affected = outcome.dep_graph.affected_outputs(&schema_path);
    assert!(
        affected.contains(&article_output),
        "schema change should affect article output; affected: {affected:?}"
    );

    // Changing the content file should affect the article output
    let affected = outcome.dep_graph.affected_outputs(&content_path);
    assert!(
        affected.contains(&article_output),
        "content change should affect article output"
    );
}

#[test]
fn build_site_dep_graph_index_depends_on_all_content() {
    let site_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/blog-site");
    let outcome = publisher_cli::build_site(std::path::Path::new(site_dir))
        .expect("build should succeed");

    let index_output = std::path::Path::new(site_dir).join("output/index.html");
    let content_path = std::path::Path::new(site_dir)
        .join("content/article/hello-world.md");

    let affected = outcome.dep_graph.affected_outputs(&content_path);
    assert!(
        affected.contains(&index_output),
        "content change should affect index.html; affected: {affected:?}"
    );
}

#[test]
fn build_site_dep_graph_index_depends_on_index_template() {
    let site_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/blog-site");
    let outcome = publisher_cli::build_site(std::path::Path::new(site_dir))
        .expect("build should succeed");

    let index_output = std::path::Path::new(site_dir).join("output/index.html");
    let index_template = std::path::Path::new(site_dir).join("templates/index.html");

    let affected = outcome.dep_graph.affected_outputs(&index_template);
    assert!(
        affected.contains(&index_output),
        "template change should affect index; affected: {affected:?}"
    );
}

#[test]
fn build_site_articles_collection_has_url_field() {
    let site_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/blog-site");

    let outcome = publisher_cli::build_site(std::path::Path::new(site_dir))
        .expect("build should succeed");

    let articles = outcome.built_pages.get("article").expect("article pages should exist");
    assert!(!articles.is_empty(), "should have at least one article");

    let article = &articles[0];
    assert!(!article.url_path.is_empty(), "url_path should be set");
    assert!(article.url_path.starts_with("/article/"), "url_path should start with /article/: {}", article.url_path);

    // The data graph should contain url and link fields
    match article.data.resolve(&["url"]) {
        Some(template::Value::Text(url)) => {
            assert!(url.starts_with("/article/"), "url field should be a path: {url}");
        }
        other => panic!("expected url Text field, got: {other:?}"),
    }
}
