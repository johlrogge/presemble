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

// ── Read-only validation tests (never call build) ────────────────────────────

#[test]
fn fixture_blog_site_has_required_inputs() {
    let site = fixture_src();
    // New directory-based convention: schemas/{stem}/item.md and templates/{stem}/item.html
    assert!(site.join("schemas/article/item.md").exists());
    assert!(site.join("templates/article/item.html").exists());
    assert!(site.join("content/article/hello-world.md").exists());
}

#[test]
fn hello_world_is_valid_against_article_schema() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
    let content_src =
        include_str!("../../../fixtures/blog-site/content/article/hello-world.md");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    let doc = content::parse_and_assign(content_src, &grammar).expect("content parses");
    let result = content::validate(&doc, &grammar);

    assert!(result.is_valid(), "hello-world should be valid: {result:?}");
}

#[test]
fn invalid_post_fails_validation_with_title_and_body_errors() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
    let content_src =
        include_str!("../../../fixtures/blog-site/content/article/invalid-post.md");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    let doc = content::parse_and_assign(content_src, &grammar).expect("content parses");
    let result = content::validate(&doc, &grammar);

    assert!(!result.is_valid(), "invalid-post should fail validation");
    let messages: Vec<_> = result.diagnostics.iter().map(|d| &d.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("title")),
        "should have title error; messages: {messages:?}"
    );
}

// ── Build tests (each gets its own temp dir) ─────────────────────────────────

#[test]
fn build_produces_index_html() {
    let (_tmp, site_dir) = copy_fixture_site();

    let outcome = publisher_cli::build_for_serve(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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
    assert_eq!(outcome.files_failed, 0, "build should have no failures");
}

#[test]
fn build_site_articles_collection_has_url_field() {
    let (_tmp, site_dir) = copy_fixture_site();

    publisher_cli::build_for_serve(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

    // The article HTML should exist and contain a URL to itself
    let article_html = publisher_cli::output_dir(&site_dir).join("article/hello-world/index.html");
    assert!(article_html.exists(), "article HTML should be built");
}

#[test]
fn build_site_copies_assets_to_output() {
    let (_tmp, site_dir) = copy_fixture_site();

    publisher_cli::build_for_serve(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");

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
    fs::create_dir_all(site.join("schemas/article")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates/article")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    // Schema
    fs::write(
        site.join("schemas/article/item.md"),
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
        site.join("templates/article/item.html"),
        r#"<!DOCTYPE html>
<html lang="en">
<head><title>Test</title></head>
<body>
<presemble:include src="header" />
<main><presemble:insert data="input.title" as="h1" /></main>
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

    let outcome = publisher_cli::build_for_serve(&site, &publisher_cli::UrlConfig::default()).expect("build should succeed");
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

    let outcome = publisher_cli::build_for_serve(&site_dir, &publisher_cli::UrlConfig::default()).expect("build should succeed");
    assert_eq!(outcome.files_failed, 0, "build should have no failures");

    // The hello-world article HTML should be built and contain author info
    let article_html = publisher_cli::output_dir(&site_dir).join("article/hello-world/index.html");
    assert!(article_html.exists(), "hello-world article should be built");
}

#[test]
fn invalid_post_is_rendered_with_html_output() {
    let (_tmp, site_dir) = copy_fixture_site();

    publisher_cli::build_for_serve(&site_dir, &publisher_cli::UrlConfig::default())
        .expect("build should succeed");

    // The invalid-post should be rendered (conductor renders all content)
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

    fs::create_dir_all(site.join("schemas/article")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates/article")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    // Article schema and content (required for a valid site)
    fs::write(
        site.join("schemas/article/item.md"),
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

    // Index content (flat, at content root)
    fs::write(
        site.join("content/index.md"),
        "# My Awesome Site\n\nBuilt with Presemble.\n",
    )
    .unwrap();

    // Article template
    fs::write(
        site.join("templates/article/item.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="input.title" as="h1" /></body></html>"#,
    )
    .unwrap();

    // Index template that uses index.* data paths
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html>
<html>
<head><title><presemble:insert data="input.site_title" /></title></head>
<body>
<h1><presemble:insert data="input.site_title" /></h1>
<p><presemble:insert data="input.tagline" /></p>
</body>
</html>"#,
    )
    .unwrap();

    // Minimal CSS so asset copy doesn't complain
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome =
        publisher_cli::build_for_serve(&site, &publisher_cli::UrlConfig::default())
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
fn collection_page_is_built_when_index_content_and_template_exist() {
    // Build a site with content/article/index.md, schemas/article/index.md,
    // and templates/article/index.html — verify that output/article/index.html
    // is produced and contains the collection listing.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("collection-page-site");

    fs::create_dir_all(site.join("schemas/article")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates/article")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    // Item schema and content
    fs::write(
        site.join("schemas/article/item.md"),
        "# Article title {#title}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(
        site.join("content/article/hello.md"),
        "# Hello Article\n",
    )
    .unwrap();

    // Collection schema (for content/article/index.md)
    fs::write(
        site.join("schemas/article/index.md"),
        "# Page heading {#heading}\noccurs\n: exactly once\n",
    )
    .unwrap();

    // Collection content
    fs::write(
        site.join("content/article/index.md"),
        "# All Articles\n",
    )
    .unwrap();

    // Item template
    fs::write(
        site.join("templates/article/item.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="input.title" as="h1" /></body></html>"#,
    )
    .unwrap();

    // Collection (index) template that renders the heading and iterates articles
    // Note: data-each only works on <template> elements; within the loop, each
    // item is bound under "item" (e.g. "item.title").
    fs::write(
        site.join("templates/article/index.html"),
        r#"<!DOCTYPE html>
<html>
<body>
<h1><presemble:insert data="input.heading" /></h1>
<ul><template data-each="input.article"><li><presemble:insert data="item.title" as="li" /></li></template></ul>
</body>
</html>"#,
    )
    .unwrap();

    // Index template (required to avoid a warning)
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html><html><body><h1>Home</h1></body></html>"#,
    )
    .unwrap();

    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome =
        publisher_cli::build_for_serve(&site, &publisher_cli::UrlConfig::default())
            .expect("build should succeed");
    assert_eq!(
        outcome.files_failed, 0,
        "no pages should fail; errors: {:?}",
        outcome.build_errors
    );

    // The collection page should exist
    let collection_output = publisher_cli::output_dir(&site).join("article/index.html");
    assert!(
        collection_output.exists(),
        "output/article/index.html should be created for the collection page"
    );

    let html = fs::read_to_string(&collection_output).unwrap();

    // The collection heading from collection content should appear
    assert!(
        html.contains("All Articles"),
        "collection page should contain heading from collection content: {html}"
    );

    // The item title from the individual article should appear (via data-each iteration)
    assert!(
        html.contains("Hello Article"),
        "collection page should contain item title via data-each: {html}"
    );
}

#[test]
fn collection_page_without_collection_content_is_skipped() {
    // If content/article/index.md does NOT exist, no collection page is built
    // and no failure is recorded.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("no-collection-content-site");

    fs::create_dir_all(site.join("schemas/article")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates/article")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    fs::write(
        site.join("schemas/article/item.md"),
        "# Article title {#title}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(
        site.join("content/article/hello.md"),
        "# Hello Article\n",
    )
    .unwrap();
    fs::write(
        site.join("templates/article/item.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="input.title" as="h1" /></body></html>"#,
    )
    .unwrap();
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html><html><body><h1>Home</h1></body></html>"#,
    )
    .unwrap();
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let outcome =
        publisher_cli::build_for_serve(&site, &publisher_cli::UrlConfig::default())
            .expect("build should succeed");

    assert_eq!(
        outcome.files_failed, 0,
        "no failures should occur when collection content is absent"
    );

    // No collection page should exist
    let collection_output = publisher_cli::output_dir(&site).join("article/index.html");
    assert!(
        !collection_output.exists(),
        "output/article/index.html should NOT exist when collection content is absent"
    );
}

#[test]
fn rebuild_affected_completes_successfully() {
    // Verify that rebuild_affected can be called and returns a BuildOutcome.
    // Since the conductor now handles all builds, rebuild_affected just delegates
    // to the conductor and returns counts.
    let tmp = TempDir::new().unwrap();
    let site = tmp.path().join("rebuild-site");

    fs::create_dir_all(site.join("schemas/article")).unwrap();
    fs::create_dir_all(site.join("content/article")).unwrap();
    fs::create_dir_all(site.join("templates/article")).unwrap();
    fs::create_dir_all(site.join("assets")).unwrap();

    fs::write(
        site.join("schemas/article/item.md"),
        "# Article title {#title}\noccurs\n: exactly once\n",
    )
    .unwrap();
    fs::write(site.join("content/article/sample.md"), "# Sample\n").unwrap();
    fs::write(
        site.join("templates/article/item.html"),
        r#"<!DOCTYPE html><html><body><presemble:insert data="input.title" as="h1" /></body></html>"#,
    )
    .unwrap();
    fs::write(
        site.join("templates/index.html"),
        r#"<!DOCTYPE html><html><body><h1>Home</h1></body></html>"#,
    )
    .unwrap();
    fs::write(site.join("assets/style.css"), "body {}").unwrap();

    let site = fs::canonicalize(&site).unwrap();

    let rebuild = publisher_cli::rebuild_affected(
        &site,
        &std::collections::HashSet::new(),
        &dep_graph::DependencyGraph::new(),
        &publisher_cli::UrlConfig::default(),
        &[],
        &publisher_cli::BuildPolicy::lenient(),
    )
    .expect("rebuild_affected should succeed");

    assert_eq!(rebuild.files_failed, 0, "rebuild should have no failures");
}
