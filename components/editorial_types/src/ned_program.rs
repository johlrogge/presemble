use crate::NedMutation;
use ned::NodeTree;

// ---------------------------------------------------------------------------
// Clojure string literal escaping
// ---------------------------------------------------------------------------
// Duplicated from conductor::conductor — intentional per architect decision.
// Revisit consolidation in Phase C6.

/// Escape a Rust string for use as a Clojure string literal body.
/// Handles `\\` and `\"`. Strips `\r` (CR) and null bytes rather than panicking.
/// Returns the escaped content without surrounding quotes.
fn clj_str_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\0' | '\r' => {} // strip CR and null
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// Wrap a Rust string as a Clojure string literal (with surrounding quotes).
pub(crate) fn clj_str(s: &str) -> String {
    format!("\"{}\"", clj_str_escape(s))
}

/// Render a [`NodeTree`] to Clojure source that reconstructs the same tree
/// at runtime using the `ned/mk-text`, `ned/mk-element`, `ned/with-attr`, and
/// `ned/with-child` primitives.
///
/// Returns `Err` if the tree contains a [`NodeTree::Existing`] node, because
/// `NodeId`s are store-local and cannot be serialised as portable Clojure.
pub(crate) fn render_node_tree(tree: &NodeTree) -> Result<String, String> {
    match tree {
        NodeTree::Text(s) => Ok(format!("(ned/mk-text {})", clj_str(s))),
        NodeTree::Existing(_) => {
            Err("NodeTree::Existing cannot be rendered to Clojure".to_string())
        }
        NodeTree::Element { name, attrs, children } => {
            // Gather all lines of the threading macro
            let mut lines: Vec<String> = Vec::new();
            lines.push(format!("(ned/mk-element {})", clj_str(name)));
            for (k, v) in attrs {
                lines.push(format!("(ned/with-attr {} {})", clj_str(k), clj_str(v)));
            }
            for child in children {
                let child_src = render_node_tree(child)?;
                lines.push(format!("(ned/with-child {})", child_src));
            }

            if lines.len() == 1 {
                // No attrs or children — no threading needed
                Ok(lines.remove(0))
            } else {
                // Build a threading macro:  (-> <expr1>\n    <expr2>\n    ...)
                let first = lines.remove(0);
                let rest = lines
                    .iter()
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(format!("(-> {first}\n{rest})"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// compose_ned_program
// ---------------------------------------------------------------------------

/// Compose a complete Clojure NED program from a selection expression and a
/// [`NedMutation`].
///
/// The returned string is ready to evaluate in the NED runtime.
/// `selection` is inserted verbatim as the right-hand side of the `let`
/// binding. Pass a complete expression such as `(slot doc "title")` — it is
/// not wrapped further.
///
/// # Errors
/// Returns `Err` if any [`NodeTree`] payload contains a [`NodeTree::Existing`]
/// variant (node IDs are store-local and cannot be serialised).
pub fn compose_ned_program(selection: &str, mutation: &NedMutation) -> Result<String, String> {
    let sel = selection;
    let body = match mutation {
        NedMutation::SetText(text) => {
            format!(
                "(ned/set-text (-> s ned/descendants ned/texts) {})",
                clj_str(text)
            )
        }
        NedMutation::SearchReplace { search, replace } => {
            format!(
                "(ned/search-replace (-> s ned/descendants ned/texts) {} {})",
                clj_str(search),
                clj_str(replace)
            )
        }
        NedMutation::Replace(trees) => {
            let rendered = render_trees(trees)?;
            format!("(ned/replace s (list {rendered}))")
        }
        NedMutation::InsertChild(trees) => {
            let rendered = render_trees(trees)?;
            format!("(doseq [t (list {rendered})] (ned/insert-child s t))")
        }
        NedMutation::InsertBefore(trees) => {
            let rendered = render_trees(trees)?;
            format!("(doseq [t (list {rendered})] (ned/insert-before s t))")
        }
        NedMutation::InsertAfter(trees) => {
            let rendered = render_trees(trees)?;
            format!("(doseq [t (list {rendered})] (ned/insert-after s t))")
        }
        NedMutation::Delete => "(ned/delete s)".to_string(),
    };
    Ok(format!("(let [s {sel}] {body})"))
}

/// Helper: render a slice of [`NodeTree`] values to a space-separated Clojure
/// source fragment. Returns `Err` on the first `NodeTree::Existing` encountered.
fn render_trees(trees: &[NodeTree]) -> Result<String, String> {
    trees
        .iter()
        .map(render_node_tree)
        .collect::<Result<Vec<_>, _>>()
        .map(|v| v.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::NodeId;

    // ── clj_str tests ─────────────────────────────────────────────────────────

    #[test]
    fn clj_str_empty() {
        assert_eq!(clj_str(""), "\"\"");
    }

    #[test]
    fn clj_str_simple() {
        assert_eq!(clj_str("hello"), "\"hello\"");
    }

    #[test]
    fn clj_str_double_quote_escaped() {
        assert_eq!(clj_str("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn clj_str_backslash_escaped() {
        assert_eq!(clj_str("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn clj_str_newline_passthrough() {
        // \n is kept as-is (Clojure allows literal newlines in strings)
        assert_eq!(clj_str("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn clj_str_cr_stripped() {
        assert_eq!(clj_str("a\rb"), "\"ab\"");
    }

    #[test]
    fn clj_str_tab_passthrough() {
        assert_eq!(clj_str("a\tb"), "\"a\tb\"");
    }

    // ── render_node_tree tests ─────────────────────────────────────────────────

    #[test]
    fn text_leaf_renders_mk_text() {
        let t = NodeTree::Text("hi".to_string());
        let src = render_node_tree(&t).expect("ok");
        assert_eq!(src, "(ned/mk-text \"hi\")");
    }

    #[test]
    fn text_leaf_with_quotes_and_backslashes() {
        let t = NodeTree::Text("say \"hi\" \\ done".to_string());
        let src = render_node_tree(&t).expect("ok");
        // Should contain properly escaped Clojure string
        assert!(src.contains("\\\"hi\\\""), "got: {src}");
        assert!(src.contains("\\\\"), "got: {src}");
    }

    #[test]
    fn element_with_one_attr_one_child() {
        let t = NodeTree::element("p")
            .with_attr("class", "intro")
            .with_child(NodeTree::text("hello"));
        let src = render_node_tree(&t).expect("ok");
        assert!(src.contains("ned/mk-element"), "got: {src}");
        assert!(src.contains("ned/with-attr"), "got: {src}");
        assert!(src.contains("ned/with-child"), "got: {src}");
        assert!(src.contains("\"p\""), "got: {src}");
        assert!(src.contains("\"class\""), "got: {src}");
        assert!(src.contains("\"intro\""), "got: {src}");
        assert!(src.contains("(ned/mk-text \"hello\")"), "got: {src}");
    }

    #[test]
    fn nested_element() {
        let inner = NodeTree::element("span").with_child(NodeTree::text("x"));
        let outer = NodeTree::element("div").with_child(inner);
        let src = render_node_tree(&outer).expect("ok");
        // Outer should contain a ned/with-child that itself contains ned/mk-element
        assert!(src.contains("ned/mk-element"), "got: {src}");
        assert!(src.contains("ned/with-child"), "got: {src}");
        // The inner element rendering should appear in the child position
        assert!(src.contains("(ned/mk-text \"x\")"), "got: {src}");
    }

    #[test]
    fn existing_node_returns_err() {
        let t = NodeTree::Existing(NodeId(7));
        let result = render_node_tree(&t);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("NodeTree::Existing cannot be rendered to Clojure")
        );
    }

    #[test]
    fn element_with_no_attrs_or_children_no_threading() {
        let t = NodeTree::element("br");
        let src = render_node_tree(&t).expect("ok");
        // No threading macro needed — just the mk-element call
        assert!(!src.starts_with("(->"), "got: {src}");
        assert_eq!(src, "(ned/mk-element \"br\")");
    }

    #[test]
    fn element_threading_macro_format() {
        let t = NodeTree::element("p").with_attr("id", "main");
        let src = render_node_tree(&t).expect("ok");
        assert!(src.starts_with("(->"), "got: {src}");
        assert!(src.contains("(ned/mk-element \"p\")"), "got: {src}");
        assert!(src.contains("(ned/with-attr \"id\" \"main\")"), "got: {src}");
    }

    // ── compose_ned_program tests ─────────────────────────────────────────────

    #[test]
    fn compose_set_text() {
        let sel = r#"(slot doc "title")"#;
        let src = compose_ned_program(sel, &NedMutation::SetText("Hello".to_string()))
            .expect("ok");
        assert!(src.contains("(ned/set-text"), "got: {src}");
        assert!(src.contains("ned/descendants"), "got: {src}");
        assert!(src.contains("ned/texts"), "got: {src}");
        assert!(src.contains("\"Hello\""), "got: {src}");
    }

    #[test]
    fn compose_set_text_escapes_special_chars() {
        let sel = r#"(slot doc "body")"#;
        // text contains: double-quote, backslash, newline
        let text = "say \"hi\" \\ line\nbreak";
        let src = compose_ned_program(sel, &NedMutation::SetText(text.to_string()))
            .expect("ok");
        // double-quote and backslash must be escaped in output
        assert!(src.contains("\\\"hi\\\""), "escaped quote missing; got: {src}");
        assert!(src.contains("\\\\"), "escaped backslash missing; got: {src}");
        // newline is kept as-is
        assert!(src.contains('\n'), "newline missing; got: {src}");
    }

    #[test]
    fn compose_search_replace() {
        let sel = r#"(slot doc "body")"#;
        let mutation = NedMutation::SearchReplace {
            search: "old".to_string(),
            replace: "new".to_string(),
        };
        let src = compose_ned_program(sel, &mutation).expect("ok");
        assert!(src.contains("(ned/search-replace"), "got: {src}");
        assert!(src.contains("\"old\""), "search string missing; got: {src}");
        assert!(src.contains("\"new\""), "replace string missing; got: {src}");
        assert!(src.contains("ned/descendants"), "got: {src}");
        assert!(src.contains("ned/texts"), "got: {src}");
    }

    #[test]
    fn compose_replace() {
        let sel = r#"(slot doc "title")"#;
        let tree = NodeTree::element("h1").with_child(NodeTree::text("New Title"));
        let src = compose_ned_program(sel, &NedMutation::Replace(vec![tree])).expect("ok");
        assert!(src.contains("(ned/replace"), "got: {src}");
        assert!(src.contains("(list"), "got: {src}");
        assert!(src.contains("ned/mk-element"), "got: {src}");
    }

    #[test]
    fn compose_insert_child() {
        let sel = r#"(slot doc "body")"#;
        let tree = NodeTree::element("p").with_child(NodeTree::text("child"));
        let src =
            compose_ned_program(sel, &NedMutation::InsertChild(vec![tree])).expect("ok");
        assert!(src.contains("doseq"), "got: {src}");
        assert!(src.contains("(ned/insert-child"), "got: {src}");
    }

    #[test]
    fn compose_insert_before() {
        let sel = r#"(slot doc "body")"#;
        let tree = NodeTree::text("before");
        let src =
            compose_ned_program(sel, &NedMutation::InsertBefore(vec![tree])).expect("ok");
        assert!(src.contains("doseq"), "got: {src}");
        assert!(src.contains("(ned/insert-before"), "got: {src}");
    }

    #[test]
    fn compose_insert_after() {
        let sel = r#"(slot doc "body")"#;
        let tree = NodeTree::text("after");
        let src =
            compose_ned_program(sel, &NedMutation::InsertAfter(vec![tree])).expect("ok");
        assert!(src.contains("doseq"), "got: {src}");
        assert!(src.contains("(ned/insert-after"), "got: {src}");
    }

    #[test]
    fn compose_delete() {
        let sel = r#"(slot doc "title")"#;
        let src = compose_ned_program(sel, &NedMutation::Delete).expect("ok");
        assert!(src.contains("(ned/delete s)"), "got: {src}");
    }

    #[test]
    fn compose_selection_verbatim_in_let_binding() {
        // The selection expression must appear verbatim — no extra wrapping parens.
        // Input already contains its own parens: `(slot doc "title")`.
        // Output should bind it as-is: `(let [s (slot doc "title")] ...)`.
        let sel = r#"(slot doc "title")"#;
        let src = compose_ned_program(sel, &NedMutation::Delete).expect("ok");
        let expected = r#"(let [s (slot doc "title")]"#;
        assert!(
            src.contains(expected),
            "selection not inserted verbatim; expected `{expected}` in: {src}"
        );
        // Must NOT produce double-wrapped form `(let [s ((slot ...)]`
        let double_wrapped = r#"(let [s ((slot doc "title")]"#;
        assert!(
            !src.contains(double_wrapped),
            "selection was double-wrapped; got: {src}"
        );
    }

    #[test]
    fn compose_replace_with_existing_node_returns_err() {
        let sel = r#"(slot doc "title")"#;
        let trees = vec![NodeTree::Existing(NodeId(7))];
        let result = compose_ned_program(sel, &NedMutation::Replace(trees));
        assert!(result.is_err(), "expected Err but got Ok");
        assert!(
            result.unwrap_err().contains("NodeTree::Existing cannot be rendered to Clojure"),
            "wrong error message"
        );
    }
}
