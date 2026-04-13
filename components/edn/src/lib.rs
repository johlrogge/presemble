/// Serialize a `template::Value` to an EDN string.
pub fn value_to_edn(value: &template::Value) -> String {
    match value {
        template::Value::Text(s) => format!("\"{}\"", escape_edn_string(s)),
        template::Value::Html(s) => format!("\"{}\"", escape_edn_string(s)),
        template::Value::Record(graph) => data_graph_to_edn(graph),
        template::Value::List(items) => {
            let inner: Vec<String> = items.iter().map(value_to_edn).collect();
            format!("[{}]", inner.join(" "))
        }
        template::Value::Absent => "nil".to_string(),
        template::Value::Suggestion { hint, .. } => {
            format!("\"<suggestion: {}>\"", escape_edn_string(hint))
        }
        template::Value::LinkExpression { .. } => "nil".to_string(),
        template::Value::Integer(n) => n.to_string(),
        template::Value::Bool(b) => b.to_string(),
        template::Value::Keyword { namespace, name } => match namespace {
            Some(ns) => format!(":{ns}/{name}"),
            None => format!(":{name}"),
        },
        template::Value::Fn(c) => format!("\"#<fn {}>\"", c.name().unwrap_or("anonymous")),
    }
}

/// Serialize a `DataGraph` to an EDN map string.
pub fn data_graph_to_edn(graph: &template::DataGraph) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for (key, value) in graph.iter() {
        // Skip internal metadata keys
        if key.starts_with('_') {
            continue;
        }
        pairs.push(format!(":{} {}", key, value_to_edn(value)));
    }
    format!("{{{}}}", pairs.join(" "))
}

fn escape_edn_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;
    use template::{DataGraph, Value};

    fn make_graph(entries: Vec<(&str, Value)>) -> DataGraph {
        let mut g = DataGraph::new();
        for (k, v) in entries {
            g.insert(k, v);
        }
        g
    }

    // --- value_to_edn ---

    #[test]
    fn text_value_is_quoted_string() {
        assert_eq!(value_to_edn(&Value::Text("hello".into())), "\"hello\"");
    }

    #[test]
    fn html_value_is_quoted_string() {
        assert_eq!(
            value_to_edn(&Value::Html("<p>hi</p>".into())),
            "\"<p>hi</p>\""
        );
    }

    #[test]
    fn absent_value_is_nil() {
        assert_eq!(value_to_edn(&Value::Absent), "nil");
    }

    #[test]
    fn suggestion_value_formats_hint() {
        let v = Value::Suggestion {
            hint: "Add a title".into(),
            slot_name: "title".into(),
            element_kind: template::SuggestionKind::Heading { level: 1 },
        };
        assert_eq!(value_to_edn(&v), "\"<suggestion: Add a title>\"");
    }

    #[test]
    fn link_expression_value_is_nil() {
        let v = Value::LinkExpression {
            text: content::LinkText::Static("label".into()),
            target: content::LinkTarget::PathRef("some/path".into()),
        };
        assert_eq!(value_to_edn(&v), "nil");
    }

    #[test]
    fn list_of_text_values() {
        let v = Value::List(vec![
            Value::Text("a".into()),
            Value::Text("b".into()),
            Value::Text("c".into()),
        ]);
        assert_eq!(value_to_edn(&v), "[\"a\" \"b\" \"c\"]");
    }

    #[test]
    fn empty_list() {
        assert_eq!(value_to_edn(&Value::List(vec![])), "[]");
    }

    #[test]
    fn record_value_serializes_as_map() {
        let graph = make_graph(vec![("title", Value::Text("My Post".into()))]);
        let v = Value::Record(graph);
        let result = value_to_edn(&v);
        assert_eq!(result, "{:title \"My Post\"}");
    }

    // --- string escaping ---

    #[test]
    fn escapes_double_quotes_in_text() {
        let v = Value::Text("say \"hello\"".into());
        assert_eq!(value_to_edn(&v), "\"say \\\"hello\\\"\"");
    }

    #[test]
    fn escapes_newlines_in_text() {
        let v = Value::Text("line1\nline2".into());
        assert_eq!(value_to_edn(&v), "\"line1\\nline2\"");
    }

    #[test]
    fn escapes_backslashes_in_text() {
        let v = Value::Text("path\\to\\file".into());
        assert_eq!(value_to_edn(&v), "\"path\\\\to\\\\file\"");
    }

    #[test]
    fn escapes_tabs_in_text() {
        let v = Value::Text("col1\tcol2".into());
        assert_eq!(value_to_edn(&v), "\"col1\\tcol2\"");
    }

    #[test]
    fn escapes_carriage_return_in_text() {
        let v = Value::Text("line\r\n".into());
        assert_eq!(value_to_edn(&v), "\"line\\r\\n\"");
    }

    // --- data_graph_to_edn ---

    #[test]
    fn empty_graph_is_empty_map() {
        let graph = DataGraph::new();
        assert_eq!(data_graph_to_edn(&graph), "{}");
    }

    #[test]
    fn graph_with_multiple_keys() {
        let graph = make_graph(vec![
            ("name", Value::Text("Alice".into())),
            ("age", Value::Text("30".into())),
        ]);
        let result = data_graph_to_edn(&graph);
        // Keys may appear in any order (im::HashMap)
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
        assert!(result.contains(":name \"Alice\""));
        assert!(result.contains(":age \"30\""));
    }

    #[test]
    fn internal_keys_are_skipped() {
        let graph = make_graph(vec![
            ("title", Value::Text("Visible".into())),
            ("_source", Value::Text("hidden".into())),
            ("_meta", Value::Absent),
        ]);
        let result = data_graph_to_edn(&graph);
        assert!(result.contains(":title"));
        assert!(!result.contains("_source"));
        assert!(!result.contains("_meta"));
    }

    // --- nested structures ---

    #[test]
    fn nested_record_containing_list_of_records() {
        let inner1 = make_graph(vec![("tag", Value::Text("rust".into()))]);
        let inner2 = make_graph(vec![("tag", Value::Text("edn".into()))]);
        let tags = Value::List(vec![Value::Record(inner1), Value::Record(inner2)]);
        let outer = make_graph(vec![("tags", tags)]);
        let result = data_graph_to_edn(&outer);
        assert!(result.contains(":tags [{:tag \"rust\"} {:tag \"edn\"}]"));
    }

    #[test]
    fn record_with_absent_field() {
        let graph = make_graph(vec![
            ("title", Value::Text("Hello".into())),
            ("subtitle", Value::Absent),
        ]);
        let result = data_graph_to_edn(&graph);
        assert!(result.contains(":subtitle nil"));
        assert!(result.contains(":title \"Hello\""));
    }
}
