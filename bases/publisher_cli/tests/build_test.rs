use std::fs;
use std::path::Path;

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
