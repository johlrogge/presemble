//! Guard for ADR-059 stage 5, at string level: a body paragraph that starts
//! with an escaped `#` renders without the backslash. It holds before the
//! stage too (the renderer read the stored source as markdown), so it is not a
//! red test: the stored text itself only changes in 5.2 and 5.4, and no API
//! that exists in both versions shows it.

#[test]
fn an_escaped_paragraph_renders_without_backslash() {
    let grammar =
        schema::parse_schema("# Title {#title}\noccurs\n: exactly once\n\n----\n\nBody.\n")
            .expect("schema parses");
    let doc = content::parse_and_assign("# T\n\n----\n\n\\# x\n", &grammar).expect("parses");
    let graph = template::build_article_graph(&doc, &grammar, template::OutputMode::Published);
    let mut context = template::DataGraph::new();
    context.insert("input", template::Value::Record(graph));
    let html = template::render_template(
        r#"<div><presemble:insert data="(ref input.body)" /></div>"#,
        &context,
        template::OutputMode::Published,
    )
    .expect("renders");
    assert!(html.contains("<p"), "{html}");
    assert!(
        html.contains("># x</p>"),
        "the paragraph shows `# x`: {html}"
    );
    assert!(!html.contains("\\#"), "the backslash is rendered: {html}");
}
