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
