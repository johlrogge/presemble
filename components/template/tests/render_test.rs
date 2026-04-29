use template;

/// End-to-end: render a schema-derived document and verify that the slot outer element
/// carries `data-presemble-schema-constraints-occurs` when the grammar specifies it.
#[test]
fn render_schema_derived_slot_emits_constraint_attributes() {
    use schema::{Constraint, CountRange, Element, Grammar, HeadingLevel, HeadingLevelRange, Slot, SlotName, Span};
    use content::parse_and_assign;

    // A minimal grammar: title slot with Occurs(Exactly(1)).
    let grammar = Grammar {
        preamble: vec![Slot {
            name: SlotName::new("title"),
            element: Element::Heading {
                level: HeadingLevelRange {
                    min: HeadingLevel::new(1).unwrap(),
                    max: HeadingLevel::new(1).unwrap(),
                },
            },
            constraints: vec![Constraint::Occurs(CountRange::Exactly(1))],
            hint_text: None,
            span: Span { start: 0, end: 0 },
        }],
        body: None,
    };

    let doc_input = "# My Article Title\n";
    let doc = parse_and_assign(doc_input, &grammar).expect("document should parse");
    let slot_graph = template::build_article_graph(&doc, &grammar);

    // Wrap in a context the same way as the real render pipeline.
    let mut context = template::DataGraph::new();
    context.insert("input", template::Value::Record(slot_graph));

    let template_src = r#"<h1><presemble:insert data="input.title" /></h1>"#;
    let html = template::render_template(template_src, &context)
        .expect("render should succeed");

    assert!(
        html.contains("data-presemble-schema-constraints-occurs=\"exactly-once\""),
        "expected data-presemble-schema-constraints-occurs attribute on title element; got: {html}"
    );
}

#[test]
fn render_article_with_dom_transformer() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
    let content_src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
    let template_src = include_str!("../../../fixtures/blog-site/templates/article/item.html");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    let doc = content::parse_and_assign(content_src, &grammar).expect("content parses");

    let slot_graph = template::build_article_graph(&doc, &grammar);
    let mut context = template::DataGraph::new();
    context.insert("input", template::Value::Record(slot_graph));

    let html = template::render_template(template_src, &context)
        .expect("render should succeed");

    // Title with semantic class
    assert!(html.contains("input-title"), "title class in output: {html}");
    assert!(html.contains("Hello, World: Getting Started With Presemble"), "title text in output: {html}");

    // Author with semantic class and link
    assert!(html.contains("input-author"), "author class in output: {html}");
    assert!(html.contains("johlrogge"), "author href in output: {html}");

    // Cover image with semantic class
    assert!(html.contains("input-cover"), "cover class in output: {html}");
    assert!(html.contains("images/cover.jpg"), "cover src in output: {html}");

    // Body content
    assert!(html.contains("What Is Presemble"), "body heading in output: {html}");
}

#[test]
fn render_template_missing_slot_produces_empty_not_error() {
    // A slot that's absent should produce empty output, not a RenderError
    let template_src = r#"<div><presemble:insert data="input.missing" /></div>"#;
    let graph = template::DataGraph::new(); // empty graph
    let html = template::render_template(template_src, &graph).expect("should not error");
    assert!(html.contains("<div>") && !html.contains("presemble"), "{html}");
}

#[test]
fn render_template_data_slot_absent_removes_block() {
    let template_src = r#"<template data-slot="input.cover"><img src="x" /></template>"#;
    let graph = template::DataGraph::new();
    let html = template::render_template(template_src, &graph).expect("should not error");
    assert!(html.is_empty() || !html.contains("img"), "{html}");
}

/// T6: attach_schema_instance_attrs adds count and sample URL to the first root element.
///
/// End-to-end: render a simple template, then attach schema-instance badge data
/// and verify the resulting HTML contains the expected data attributes.
#[test]
fn attach_schema_instance_attrs_emits_count_and_sample_url() {
    // Render a minimal template to get a node list.
    let template_src = r#"<div class="page"><h1>Schema Page</h1></div>"#;
    let graph = template::DataGraph::new();
    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let transformed = template::transform(nodes, &graph, &ctx).expect("transform");

    // Attach badge data: 3 documents, first is /article/hello-world
    let with_badge = template::attach_schema_instance_attrs(
        transformed,
        3,
        Some("/article/hello-world"),
    );

    let html = template::serialize_nodes(&with_badge);

    assert!(
        html.contains(r#"data-presemble-schema-instance-count="3""#),
        "expected instance count attr; got: {html}"
    );
    assert!(
        html.contains(r#"data-presemble-schema-instance-sample-url="/article/hello-world""#),
        "expected sample URL attr; got: {html}"
    );
}

/// T6 edge case: count=0 emits count attr but NOT the sample URL attr.
#[test]
fn attach_schema_instance_attrs_zero_count_omits_sample_url() {
    let template_src = r#"<div class="page"><h1>Empty Schema</h1></div>"#;
    let graph = template::DataGraph::new();
    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let transformed = template::transform(nodes, &graph, &ctx).expect("transform");

    // Attach badge data: 0 documents, no sample URL
    let with_badge = template::attach_schema_instance_attrs(transformed, 0, None);

    let html = template::serialize_nodes(&with_badge);

    assert!(
        html.contains(r#"data-presemble-schema-instance-count="0""#),
        "expected count=0 attr; got: {html}"
    );
    assert!(
        !html.contains("data-presemble-schema-instance-sample-url"),
        "expected NO sample URL attr when count=0; got: {html}"
    );
}

/// T6 integration: verify the attrs land on the document root element, not on child elements.
#[test]
fn attach_schema_instance_attrs_targets_root_element_only() {
    let template_src = r#"<html><body><p>content</p></body></html>"#;
    let graph = template::DataGraph::new();
    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let transformed = template::transform(nodes, &graph, &ctx).expect("transform");

    let with_badge =
        template::attach_schema_instance_attrs(transformed, 1, Some("/post/first-post"));

    let html = template::serialize_nodes(&with_badge);

    // The count attr should appear exactly once (on the root <html> element).
    let count_occurrences = html
        .matches("data-presemble-schema-instance-count")
        .count();
    assert_eq!(
        count_occurrences, 1,
        "count attr should appear exactly once; got: {html}"
    );

    // It should be on the <html> element, which is first in the output.
    let count_attr_pos = html
        .find("data-presemble-schema-instance-count")
        .expect("attr must exist");
    let html_tag_pos = html.find("<html").expect("<html must exist");
    assert!(
        count_attr_pos > html_tag_pos,
        "count attr should come after <html tag open"
    );
    // It should come before the first child element.
    let body_pos = html.find("<body").expect("<body must exist");
    assert!(
        count_attr_pos < body_pos,
        "count attr should be on the root, before any child element"
    );
}

/// T7: attach_schema_included_by_attr adds the JSON back-reference list to the root element.
///
/// End-to-end: render a simple template, attach included-by data, verify the attribute.
#[test]
fn attach_schema_included_by_attr_emits_json_on_root_element() {
    let template_src = r#"<div class="schema-page"><h1>Author Schema</h1></div>"#;
    let graph = template::DataGraph::new();
    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let transformed = template::transform(nodes, &graph, &ctx).expect("transform");

    // post and event both have TypeLink("author") — so they "include" this schema
    let with_back_refs = template::attach_schema_included_by_attr(
        transformed,
        &[("event", "/event/#_schema"), ("post", "/post/#_schema")],
    );

    let html = template::serialize_nodes(&with_back_refs);

    assert!(
        html.contains("data-presemble-schema-included-by="),
        "expected data-presemble-schema-included-by attribute; got: {html}"
    );
    assert!(
        html.contains(r#""schema":"post""#) || html.contains(r#"&quot;schema&quot;:&quot;post&quot;"#),
        "expected post entry in included-by JSON; got: {html}"
    );
    assert!(
        html.contains(r#""schema":"event""#) || html.contains(r#"&quot;schema&quot;:&quot;event&quot;"#),
        "expected event entry in included-by JSON; got: {html}"
    );
}

/// T7 edge case: schema with no dependents emits `data-presemble-schema-included-by="[]"`.
#[test]
fn attach_schema_included_by_attr_empty_emits_empty_json_array() {
    let template_src = r#"<div class="schema-page"></div>"#;
    let graph = template::DataGraph::new();
    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let transformed = template::transform(nodes, &graph, &ctx).expect("transform");

    let with_back_refs = template::attach_schema_included_by_attr(transformed, &[]);

    let html = template::serialize_nodes(&with_back_refs);

    assert!(
        html.contains("data-presemble-schema-included-by="),
        "expected data-presemble-schema-included-by attribute even for empty list; got: {html}"
    );
    // The attribute value should be "[]" (HTML-escaped or not)
    assert!(
        html.contains(r#"data-presemble-schema-included-by="[]""#),
        "expected empty JSON array value; got: {html}"
    );
}

/// T7 integration: verify the attr lands on the document root element, not on child elements.
#[test]
fn attach_schema_included_by_attr_targets_root_element_only() {
    let template_src = r#"<html><body><p>content</p></body></html>"#;
    let graph = template::DataGraph::new();
    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let transformed = template::transform(nodes, &graph, &ctx).expect("transform");

    let with_back_refs =
        template::attach_schema_included_by_attr(transformed, &[("post", "/post/#_schema")]);

    let html = template::serialize_nodes(&with_back_refs);

    // The attr should appear exactly once (on the root <html> element).
    let attr_count = html.matches("data-presemble-schema-included-by").count();
    assert_eq!(
        attr_count, 1,
        "included-by attr should appear exactly once; got: {html}"
    );

    // It should be on the <html> element (before <body>).
    let attr_pos = html
        .find("data-presemble-schema-included-by")
        .expect("attr must exist");
    let body_pos = html.find("<body").expect("<body must exist");
    assert!(
        attr_pos < body_pos,
        "included-by attr should be on the root element, before any child element"
    );
}

/// End-to-end: a TypeLink slot rendered from a schema-derived document should
/// produce `<a href="/author/#_schema">` — pointing at the schema URL for that
/// linked schema type — rather than the placeholder `href="#"`.
#[test]
fn render_typelink_slot_produces_schema_href() {
    use schema::{Constraint, Element, Grammar, Slot, SlotName, Span};
    use content::parse_and_assign;

    // Grammar: one link slot named "author" with TypeLink("author") constraint.
    let grammar = Grammar {
        preamble: vec![Slot {
            name: SlotName::new("author"),
            element: Element::Link { pattern: String::new() },
            constraints: vec![Constraint::TypeLink("author".to_string())],
            hint_text: Some("Link to the author".to_string()),
            span: Span { start: 0, end: 0 },
        }],
        body: None,
    };

    // Synthesize an empty document (as schema-mode does).
    let doc_input = "---\n";
    let doc = parse_and_assign(doc_input, &grammar).expect("empty doc should parse");
    let slot_graph = template::build_article_graph(&doc, &grammar);

    let mut context = template::DataGraph::new();
    context.insert("input", template::Value::Record(slot_graph));

    let template_src = r#"<div><presemble:insert data="input.author" /></div>"#;
    let html = template::render_template(template_src, &context)
        .expect("render should succeed");

    assert!(
        html.contains(r#"href="/author/#_schema""#),
        "expected href=\"/author/#_schema\" for TypeLink slot; got: {html}"
    );
}

/// Integration test: the full schema-mode post-processing pipeline (T6 + T7 together).
///
/// Renders a schema-derived document via the template, then applies both
/// `attach_schema_instance_attrs` and `attach_schema_included_by_attr` in sequence
/// — mirroring what `Conductor::render_page` does in the `RenderMode::Schema` arm.
/// Asserts that both `data-presemble-schema-instance-count` and
/// `data-presemble-schema-included-by` appear in the serialized HTML.
#[test]
fn schema_mode_post_processors_emit_both_attrs_in_sequence() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article/item.md");
    let template_src = include_str!("../../../fixtures/blog-site/templates/article/item.html");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    // Build a minimal document from the schema using the content parser —
    // same as the non-schema-mode tests.  Schema mode only needs the graph
    // structure; the exact field values don't matter for this test.
    let content_src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");
    let doc = content::parse_and_assign(content_src, &grammar).expect("content parses");
    let article_graph = template::build_article_graph(&doc, &grammar);

    let mut context = template::DataGraph::new();
    context.insert("_presemble_stem", template::Value::Text("article".to_string()));
    context.insert("_presemble_file", template::Value::Text(String::new()));
    context.insert("url", template::Value::Text("/article/hello-world".to_string()));
    context.insert("input", template::Value::Record(article_graph));

    let nodes = template::parse_template_xml(template_src).expect("template parse");
    let reg = template::NullRegistry;
    let ctx = template::RenderContext::new(&reg);
    let (nodes, _local_defs) = template::extract_definitions(nodes);
    let transformed = template::transform(nodes, &context, &ctx).expect("transform");

    // T6: attach instance-count and sample URL
    let transformed = template::attach_schema_instance_attrs(
        transformed,
        2,
        Some("/article/hello-world"),
    );

    // T7: attach included-by back-references
    let transformed = template::attach_schema_included_by_attr(
        transformed,
        &[("post", "/post/#_schema")],
    );

    let html = template::serialize_nodes(&transformed);

    assert!(
        html.contains("data-presemble-schema-instance-count=\"2\""),
        "expected data-presemble-schema-instance-count=\"2\" in schema-mode HTML; got: {html}"
    );
    assert!(
        html.contains("data-presemble-schema-included-by="),
        "expected data-presemble-schema-included-by attribute in schema-mode HTML; got: {html}"
    );
}
