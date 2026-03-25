use template::{build_article_graph, parse_template, render, DataGraph, FileTemplateLoader, Value};

#[test]
fn render_article_produces_title_and_body() {
    let schema_src = include_str!("../../../fixtures/blog-site/schemas/article.md");
    let content_src = include_str!("../../../fixtures/blog-site/content/article/hello-world.md");

    let grammar = schema::parse_schema(schema_src).expect("schema parses");
    let doc = content::parse_document(content_src).expect("content parses");

    let article_graph = build_article_graph(&doc, &grammar);

    // The article.html template uses `article:*` paths, so wrap the article data
    // under the "article" namespace in the top-level graph.
    let mut graph = DataGraph::new();
    graph.insert("article", Value::Record(article_graph));

    let template_src = include_str!("../../../fixtures/blog-site/templates/article.html");
    let template = parse_template(template_src).expect("template parses");

    let loader = FileTemplateLoader::new("../../fixtures/blog-site/templates");
    let html = render(&template, &graph, &loader).expect("render succeeds");

    assert!(
        html.contains("Hello, World: Getting Started With Presemble"),
        "title in output; html was:\n{html}"
    );
    assert!(html.contains("johlrogge"), "author in output; html was:\n{html}");
    assert!(
        html.contains("What Is Presemble"),
        "body heading in output; html was:\n{html}"
    );
}
