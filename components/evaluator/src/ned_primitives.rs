use std::sync::{Arc, RwLock};
use template::Value;
use node_store::{Node, NodeStore};
use ned::Selection;
use ned::NodeTree;
use ned::selection;
use schema::Grammar;
use crate::closure::PrimitiveFn;
use crate::env::RootEnv;
use crate::doc_registry::{DocEntry, DocSource};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn extract_store(val: &Value) -> Result<Arc<RwLock<NodeStore>>, String> {
    match val {
        Value::Opaque(any) => {
            any.downcast_ref::<Arc<RwLock<NodeStore>>>()
                .cloned()
                .ok_or_else(|| "expected a NodeStore".into())
        }
        _ => Err("expected a NodeStore".into()),
    }
}

pub fn extract_selection(val: &Value) -> Result<Selection, String> {
    match val {
        Value::Opaque(any) => {
            any.downcast_ref::<Selection>()
                .cloned()
                .ok_or_else(|| "expected a Selection".into())
        }
        _ => Err("expected a Selection".into()),
    }
}

pub fn wrap_selection(sel: Selection) -> Value {
    Value::Opaque(Arc::new(sel))
}

pub fn wrap_store(store: Arc<RwLock<NodeStore>>) -> Value {
    Value::Opaque(Arc::new(store))
}

pub fn extract_node_tree(val: &Value) -> Result<NodeTree, String> {
    match val {
        Value::Opaque(any) => {
            any.downcast_ref::<NodeTree>()
                .cloned()
                .ok_or_else(|| "expected a NodeTree".into())
        }
        _ => Err("expected a NodeTree".into()),
    }
}

pub fn wrap_node_tree(tree: NodeTree) -> Value {
    Value::Opaque(Arc::new(tree))
}

/// Extract a `Vec<NodeTree>` from a `Value::List` of Opaque-wrapped NodeTrees.
pub fn extract_node_tree_list(val: &Value) -> Result<Vec<NodeTree>, String> {
    match val {
        Value::List(items) => items
            .iter()
            .map(extract_node_tree)
            .collect::<Result<Vec<_>, _>>(),
        _ => Err("expected a list of NodeTrees".into()),
    }
}

pub fn wrap_grammar(grammar: Arc<Grammar>) -> Value {
    Value::Opaque(Arc::new(grammar))
}

pub fn extract_grammar(val: &Value) -> Result<Arc<Grammar>, String> {
    match val {
        Value::Opaque(any) => {
            any.downcast_ref::<Arc<Grammar>>()
                .cloned()
                .ok_or_else(|| "expected a Grammar".into())
        }
        _ => Err("expected a Grammar".into()),
    }
}

fn reg(
    root: &RootEnv,
    name: &str,
    sig: &str,
    doc: &str,
    func: impl Fn(Vec<Value>) -> Result<Value, String> + Send + Sync + 'static,
) {
    let prim = PrimitiveFn::new(name, func);
    root.def_with_doc(
        name,
        Value::Fn(Arc::new(prim)),
        DocEntry {
            name: name.to_string(),
            doc: doc.to_string(),
            arglists: vec![sig.to_string()],
            source: DocSource::Primitive,
        },
    );
}

// ---------------------------------------------------------------------------
// Registration entry point
// ---------------------------------------------------------------------------

/// Register NED primitives as core evaluator builtins.
/// The store is shared via Arc<RwLock<NodeStore>>.
pub fn register_ned_builtins(root: &RootEnv, store: Arc<RwLock<NodeStore>>) {
    // ── Store access ─────────────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/store", "(ned/store)", "Return the current node store.", move |_args| {
        Ok(wrap_store(s.clone()))
    });

    // ── Selection constructors ────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/all", "(ned/all)", "Select all nodes in the store.", move |_args| {
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(Selection::all(&st)))
    });

    let s = store.clone();
    reg(root, "ned/roots", "(ned/roots)", "Select root nodes (no parents).", move |_args| {
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(Selection::roots(&st)))
    });

    // ── Traversal ─────────────────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/children", "(ned/children sel)", "Replace selection with children of selected nodes.", move |args| {
        if args.is_empty() { return Err("ned/children requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.children(&st)))
    });

    let s = store.clone();
    reg(root, "ned/parents", "(ned/parents sel)", "Replace selection with parents of selected nodes.", move |args| {
        if args.is_empty() { return Err("ned/parents requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.parents(&st)))
    });

    let s = store.clone();
    reg(root, "ned/siblings", "(ned/siblings sel)", "Replace selection with siblings of selected nodes.", move |args| {
        if args.is_empty() { return Err("ned/siblings requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.siblings(&st)))
    });

    let s = store.clone();
    reg(root, "ned/descendants", "(ned/descendants sel)", "Replace selection with all descendants of selected nodes.", move |args| {
        if args.is_empty() { return Err("ned/descendants requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.descendants(&st)))
    });

    let s = store.clone();
    reg(root, "ned/ancestors", "(ned/ancestors sel)", "Replace selection with all ancestors of selected nodes.", move |args| {
        if args.is_empty() { return Err("ned/ancestors requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.ancestors(&st)))
    });

    // ── Filter ────────────────────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/filter", "(ned/filter sel & predicates)", "Filter selection by predicate.", move |args| {
        if args.is_empty() { return Err("ned/filter requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;

        if args.len() < 2 { return Ok(wrap_selection(sel)); }

        match &args[1] {
            Value::Keyword { namespace: None, name } => {
                match name.as_str() {
                    "text" => Ok(wrap_selection(sel.filter(&st, selection::is_text()))),
                    "element" => Ok(wrap_selection(sel.filter(&st, selection::is_any_element()))),
                    "kind" => {
                        let kind_name = match args.get(2) {
                            Some(Value::Text(s)) => s.clone(),
                            _ => return Err("ned/filter :kind requires a string name".into()),
                        };
                        Ok(wrap_selection(sel.filter(&st, selection::is_element(&kind_name))))
                    }
                    "attr" => {
                        if args.len() < 4 { return Err("ned/filter :attr requires name and value".into()); }
                        let attr_name = match &args[2] {
                            Value::Text(s) => s.clone(),
                            _ => return Err("ned/filter :attr name must be a string".into()),
                        };
                        match &args[3] {
                            Value::Text(v) => Ok(wrap_selection(sel.filter(&st, selection::has_attr(&attr_name, v)))),
                            Value::Integer(v) => Ok(wrap_selection(sel.filter(&st, selection::has_attr_int(&attr_name, *v)))),
                            _ => Err("ned/filter :attr value must be string or integer".into()),
                        }
                    }
                    "text-contains" => {
                        let needle = match args.get(2) {
                            Some(Value::Text(s)) => s.clone(),
                            _ => return Err("ned/filter :text-contains requires a string".into()),
                        };
                        Ok(wrap_selection(sel.filter(&st, selection::has_text_containing(&needle))))
                    }
                    "text-equals" => {
                        let value = match args.get(2) {
                            Some(Value::Text(s)) => s.clone(),
                            _ => return Err("ned/filter :text-equals requires a string".into()),
                        };
                        Ok(wrap_selection(sel.filter(&st, selection::has_text_equals(&value))))
                    }
                    other => Err(format!("ned/filter: unknown predicate keyword :{other}")),
                }
            }
            _ => Err("ned/filter: second argument must be a keyword predicate".into()),
        }
    });

    // ── Inspection ────────────────────────────────────────────────────────────

    reg(root, "ned/count", "(ned/count sel)", "Return the number of selected nodes.", |args| {
        if args.is_empty() { return Err("ned/count requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        Ok(Value::Integer(sel.len() as i64))
    });

    reg(root, "ned/empty?", "(ned/empty? sel)", "True if selection is empty.", |args| {
        if args.is_empty() { return Err("ned/empty? requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        Ok(Value::Bool(sel.is_empty()))
    });

    let s = store.clone();
    reg(root, "ned/node-count", "(ned/node-count)", "Total number of nodes in the store.", move |_args| {
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(Value::Integer(st.node_count() as i64))
    });

    let s = store.clone();
    reg(root, "ned/edge-count", "(ned/edge-count)", "Total number of edges in the store.", move |_args| {
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(Value::Integer(st.edge_count() as i64))
    });

    // ── Node content access ───────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/text-of", "(ned/text-of sel)", "Get text content of selected Text nodes.", move |args| {
        if args.is_empty() { return Err("ned/text-of requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        let texts: Vec<Value> = sel.iter()
            .filter_map(|id| {
                if let Some(Node::Text(s)) = st.get(id) {
                    Some(Value::Text(s.clone()))
                } else {
                    None
                }
            })
            .collect();
        Ok(Value::List(texts))
    });

    let s = store.clone();
    reg(root, "ned/kind-of", "(ned/kind-of sel)", "Get element names of selected Element nodes.", move |args| {
        if args.is_empty() { return Err("ned/kind-of requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        let kinds: Vec<Value> = sel.iter()
            .filter_map(|id| {
                if let Some(Node::Element(name)) = st.get(id) {
                    Some(Value::Text(st.resolve_name(*name).to_string()))
                } else {
                    None
                }
            })
            .collect();
        Ok(Value::List(kinds))
    });

    // ── Set operations ────────────────────────────────────────────────────────

    reg(root, "ned/union", "(ned/union sel1 sel2)", "Union of two selections.", |args| {
        if args.len() < 2 { return Err("ned/union requires two selections".into()); }
        let a = extract_selection(&args[0])?;
        let b = extract_selection(&args[1])?;
        Ok(wrap_selection(a.union(&b)))
    });

    reg(root, "ned/intersection", "(ned/intersection sel1 sel2)", "Intersection of two selections.", |args| {
        if args.len() < 2 { return Err("ned/intersection requires two selections".into()); }
        let a = extract_selection(&args[0])?;
        let b = extract_selection(&args[1])?;
        Ok(wrap_selection(a.intersection(&b)))
    });

    reg(root, "ned/difference", "(ned/difference sel1 sel2)", "Difference of two selections.", |args| {
        if args.len() < 2 { return Err("ned/difference requires two selections".into()); }
        let a = extract_selection(&args[0])?;
        let b = extract_selection(&args[1])?;
        Ok(wrap_selection(a.difference(&b)))
    });

    // ── Attribute access ──────────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/attr-of", "(ned/attr-of sel attr-name)", "Get attribute values of selected nodes by attribute name.", move |args| {
        if args.len() < 2 { return Err("ned/attr-of requires selection and attribute name".into()); }
        let sel = extract_selection(&args[0])?;
        let attr_name = match &args[1] {
            Value::Text(s) => s.clone(),
            Value::Keyword { name, .. } => name.clone(),
            _ => return Err("ned/attr-of: attribute name must be a string or keyword".into()),
        };
        let st = s.read().map_err(|e| e.to_string())?;
        let values: Vec<Value> = sel.iter()
            .filter_map(|id| {
                for (name, vid) in st.attributes(id) {
                    if st.resolve_name(name) == attr_name {
                        return match st.get(vid) {
                            Some(Node::Text(s)) => Some(Value::Text(s.clone())),
                            Some(Node::Integer(n)) => Some(Value::Integer(*n)),
                            Some(Node::Boolean(b)) => Some(Value::Bool(*b)),
                            _ => None,
                        };
                    }
                }
                None
            })
            .collect();
        Ok(Value::List(values))
    });

    // ── Positional child access ───────────────────────────────────────────────

    let s = store.clone();
    // Intended for single-parent selections; behaviour across multi-parent selections
    // is well-defined but flat (all children concatenated in insertion order).
    reg(root, "ned/nth-child", "(ned/nth-child sel idx)", "Return the idx-th insertion-order child of the single node in sel.", move |args| {
        if args.len() < 2 { return Err("ned/nth-child requires a selection and an index".into()); }
        let sel = extract_selection(&args[0])?;
        let idx = match &args[1] {
            Value::Integer(n) => *n as usize,
            _ => return Err("ned/nth-child: idx must be an integer".into()),
        };
        let st = s.read().map_err(|e| e.to_string())?;
        // Collect all insertion-order children across all selected nodes.
        // Typically called on a single-node selection (the body element).
        let children: Vec<_> = sel.iter()
            .flat_map(|id| st.children(id))
            .collect();
        match children.get(idx) {
            Some(&child_id) => Ok(wrap_selection(Selection::single(child_id))),
            None => Ok(wrap_selection(Selection::new())),
        }
    });

    // ── Directional sibling traversal ────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/following-siblings", "(ned/following-siblings sel)", "Siblings strictly after each selected node in parent's child list.", move |args| {
        if args.is_empty() { return Err("ned/following-siblings requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.following_siblings(&st)))
    });

    let s = store.clone();
    reg(root, "ned/preceding-siblings", "(ned/preceding-siblings sel)", "Siblings strictly before each selected node in parent's child list.", move |args| {
        if args.is_empty() { return Err("ned/preceding-siblings requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.preceding_siblings(&st)))
    });

    let s = store.clone();
    reg(root, "ned/nth-of-kind", "(ned/nth-of-kind sel name idx)", "idx-th element of given kind within sel (caller pre-flattens with ned/children or ned/following-siblings).", move |args| {
        if args.len() < 3 { return Err("ned/nth-of-kind requires a selection, kind name, and index".into()); }
        let sel = extract_selection(&args[0])?;
        let name = match &args[1] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/nth-of-kind: kind name must be a string".into()),
        };
        let idx = match &args[2] {
            Value::Integer(n) if *n < 0 => return Err("ned/nth-of-kind: idx must be non-negative".into()),
            Value::Integer(n) => *n as usize,
            _ => return Err("ned/nth-of-kind: idx must be an integer".into()),
        };
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.nth_of_kind(&st, &name, idx)))
    });

    // ── Mutations ─────────────────────────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/set-text", "(ned/set-text sel text)", "Replace text content of selected Text nodes.", move |args| {
        if args.len() < 2 { return Err("ned/set-text requires selection and text".into()); }
        let sel = extract_selection(&args[0])?;
        let text = match &args[1] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/set-text: second argument must be a string".into()),
        };
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::set_text(&mut st, &sel, &text);
        Ok(wrap_selection(result))
    });

    let s = store.clone();
    reg(root, "ned/search-replace", "(ned/search-replace sel search replace)", "Replace all occurrences of search with replace in every Text descendant of sel.", move |args| {
        if args.len() < 3 { return Err("ned/search-replace requires selection, search, and replace".into()); }
        let sel = extract_selection(&args[0])?;
        let search = match &args[1] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/search-replace: search must be a string".into()),
        };
        let replace = match &args[2] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/search-replace: replace must be a string".into()),
        };
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::search_replace_text(&mut st, &sel, &search, &replace);
        Ok(wrap_selection(result))
    });

    let s = store.clone();
    reg(root, "ned/delete", "(ned/delete sel)", "Delete selected nodes and their descendants.", move |args| {
        if args.is_empty() { return Err("ned/delete requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::delete(&mut st, &sel);
        Ok(wrap_selection(result))
    });

    // ── NodeTree constructors ─────────────────────────────────────────────────

    reg(root, "ned/mk-text", "(ned/mk-text s)", "Construct a text NodeTree leaf.", |args| {
        if args.is_empty() { return Err("ned/mk-text requires a string".into()); }
        let s = match &args[0] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/mk-text: argument must be a string".into()),
        };
        Ok(wrap_node_tree(NodeTree::text(s)))
    });

    reg(root, "ned/mk-element", "(ned/mk-element name)", "Construct an empty element NodeTree.", |args| {
        if args.is_empty() { return Err("ned/mk-element requires a name".into()); }
        let name = match &args[0] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/mk-element: argument must be a string".into()),
        };
        Ok(wrap_node_tree(NodeTree::element(name)))
    });

    reg(root, "ned/with-child", "(ned/with-child tree child)", "Append a child NodeTree.", |args| {
        if args.len() < 2 { return Err("ned/with-child requires tree and child".into()); }
        let tree = extract_node_tree(&args[0])?;
        let child = extract_node_tree(&args[1])?;
        Ok(wrap_node_tree(tree.with_child(child)))
    });

    reg(root, "ned/with-attr", "(ned/with-attr tree name value)", "Add an attribute to a NodeTree.", |args| {
        if args.len() < 3 { return Err("ned/with-attr requires tree, name, and value".into()); }
        let tree = extract_node_tree(&args[0])?;
        let name = match &args[1] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/with-attr: name must be a string".into()),
        };
        let value = match &args[2] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/with-attr: value must be a string".into()),
        };
        Ok(wrap_node_tree(tree.with_attr(name, value)))
    });

    // ── Tree-based mutation bindings ──────────────────────────────────────────

    let s = store.clone();
    reg(root, "ned/insert-child", "(ned/insert-child sel tree)", "Insert a NodeTree as the last child under selected elements.", move |args| {
        if args.len() < 2 { return Err("ned/insert-child requires selection and tree".into()); }
        let sel = extract_selection(&args[0])?;
        let tree = extract_node_tree(&args[1])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::insert_child_tree(&mut st, &sel, tree);
        Ok(wrap_selection(result))
    });

    let s = store.clone();
    reg(root, "ned/insert-before", "(ned/insert-before sel tree)", "Insert a NodeTree before each selected node.", move |args| {
        if args.len() < 2 { return Err("ned/insert-before requires selection and tree".into()); }
        let sel = extract_selection(&args[0])?;
        let tree = extract_node_tree(&args[1])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::insert_before_tree(&mut st, &sel, tree);
        Ok(wrap_selection(result))
    });

    let s = store.clone();
    reg(root, "ned/insert-after", "(ned/insert-after sel tree)", "Insert a NodeTree after each selected node.", move |args| {
        if args.len() < 2 { return Err("ned/insert-after requires selection and tree".into()); }
        let sel = extract_selection(&args[0])?;
        let tree = extract_node_tree(&args[1])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::insert_after_tree(&mut st, &sel, tree);
        Ok(wrap_selection(result))
    });

    let s = store.clone();
    reg(root, "ned/replace", "(ned/replace sel trees)", "Replace selected nodes with a list of NodeTrees.", move |args| {
        if args.len() < 2 { return Err("ned/replace requires selection and list of trees".into()); }
        let sel = extract_selection(&args[0])?;
        let trees = extract_node_tree_list(&args[1])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::replace(&mut st, &sel, trees);
        Ok(wrap_selection(result))
    });

    // ── Grammar and body-fragment parsing ─────────────────────────────────────

    reg(root, "ned/parse-grammar", "(ned/parse-grammar schema-src)", "Parse a schema source string into an opaque Grammar value.", |args| {
        if args.is_empty() { return Err("ned/parse-grammar requires a schema source string".into()); }
        let src = match &args[0] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/parse-grammar: argument must be a string".into()),
        };
        let grammar = schema::parse_schema(&src)
            .map_err(|e| format!("ned/parse-grammar: schema parse error: {e}"))?;
        Ok(wrap_grammar(Arc::new(grammar)))
    });

    let s = store.clone();
    reg(root, "ned/parse-body", "(ned/parse-body md-source grammar)", "Parse a markdown body fragment into a list of NodeTrees (already stored).", move |args| {
        if args.len() < 2 { return Err("ned/parse-body requires markdown source and grammar".into()); }
        let md_src = match &args[0] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/parse-body: first argument must be a string".into()),
        };
        let grammar = extract_grammar(&args[1])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let ids = node_store_bridge::ingest_body_fragment(&md_src, &grammar, &mut st)
            .map_err(|e| format!("ned/parse-body: {e}"))?;
        let trees: Vec<Value> = ids
            .into_iter()
            .map(|id| wrap_node_tree(NodeTree::Existing(id)))
            .collect();
        Ok(Value::List(trees))
    });

}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::{Edge, Node, NodeStore};

    fn make_store_with_tree() -> Arc<RwLock<NodeStore>> {
        let mut store = NodeStore::new();
        let doc_name = store.intern("document");
        let h_name = store.intern("heading");
        let p_name = store.intern("paragraph");
        let level_name = store.intern("level");

        let root = store.add_node(Node::Element(doc_name));
        let h1 = store.add_node(Node::Element(h_name));
        let level1_val = store.add_node(Node::Integer(1));
        store.add_edge(h1, Edge::Attribute { name: level_name, value: level1_val });
        let text1 = store.add_node(Node::Text("Hello".to_string()));
        store.add_edge(h1, Edge::Child(text1));

        let p1 = store.add_node(Node::Element(p_name));
        let text2 = store.add_node(Node::Text("World".to_string()));
        store.add_edge(p1, Edge::Child(text2));

        store.add_edge(root, Edge::Child(h1));
        store.add_edge(root, Edge::Child(p1));

        Arc::new(RwLock::new(store))
    }

    fn setup(store: Arc<RwLock<NodeStore>>) -> RootEnv {
        let root = RootEnv::new();
        register_ned_builtins(&root, store);
        root
    }

    fn call(root: &RootEnv, name: &str, args: Vec<Value>) -> Result<Value, String> {
        match root.get(name) {
            Some(Value::Fn(f)) => f.call(args),
            _ => Err(format!("not found: {name}")),
        }
    }

    #[test]
    fn ned_all_returns_selection() {
        let store = make_store_with_tree();
        let node_count = store.read().unwrap().node_count();
        let root = setup(store);
        let sel_val = call(&root, "ned/all", vec![]).unwrap();
        let sel = match &sel_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(sel.len(), node_count);
    }

    #[test]
    fn ned_roots_returns_root_node() {
        let store = make_store_with_tree();
        let root = setup(store);
        let sel_val = call(&root, "ned/roots", vec![]).unwrap();
        let sel = match &sel_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        // The document root is the only node with no parents.
        // Integer attribute-value nodes are also roots (no Child-edge parents).
        assert!(sel.len() >= 1);
    }

    #[test]
    fn ned_count_returns_integer() {
        let store = make_store_with_tree();
        let node_count = store.read().unwrap().node_count() as i64;
        let root = setup(store.clone());
        let all = call(&root, "ned/all", vec![]).unwrap();
        let count = call(&root, "ned/count", vec![all]).unwrap();
        assert!(matches!(count, Value::Integer(n) if n == node_count));
    }

    #[test]
    fn ned_empty_is_false_for_nonempty_selection() {
        let store = make_store_with_tree();
        let root = setup(store);
        let all = call(&root, "ned/all", vec![]).unwrap();
        let result = call(&root, "ned/empty?", vec![all]).unwrap();
        assert!(matches!(result, Value::Bool(false)));
    }

    #[test]
    fn ned_filter_kind_returns_headings() {
        let store = make_store_with_tree();
        let root = setup(store);
        let all = call(&root, "ned/all", vec![]).unwrap();
        let heading_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let heading_name = Value::Text("heading".to_string());
        let sel_val = call(&root, "ned/filter", vec![all, heading_kw, heading_name]).unwrap();
        let sel = match &sel_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(sel.len(), 1); // one heading in our tree
    }

    #[test]
    fn ned_node_count_matches_store() {
        let store = make_store_with_tree();
        let expected = store.read().unwrap().node_count() as i64;
        let root = setup(store);
        let count = call(&root, "ned/node-count", vec![]).unwrap();
        assert!(matches!(count, Value::Integer(n) if n == expected));
    }

    #[test]
    fn ned_text_of_returns_texts() {
        let store = make_store_with_tree();
        let root = setup(store);
        let all = call(&root, "ned/all", vec![]).unwrap();
        let text_kw = Value::Keyword { namespace: None, name: "text".to_string() };
        let texts_sel = call(&root, "ned/filter", vec![all, text_kw]).unwrap();
        let texts = call(&root, "ned/text-of", vec![texts_sel]).unwrap();
        match texts {
            Value::List(items) => {
                assert_eq!(items.len(), 2); // "Hello" and "World"
                let strings: Vec<_> = items.iter().filter_map(|v| {
                    if let Value::Text(s) = v { Some(s.as_str()) } else { None }
                }).collect();
                assert!(strings.contains(&"Hello"));
                assert!(strings.contains(&"World"));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ned_children_traversal() {
        let store = make_store_with_tree();
        let root = setup(store);
        let all = call(&root, "ned/all", vec![]).unwrap();
        // filter to document elements, then get children
        let doc_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let doc_name = Value::Text("document".to_string());
        let docs = call(&root, "ned/filter", vec![all, doc_kw, doc_name]).unwrap();
        let children = call(&root, "ned/children", vec![docs]).unwrap();
        let sel = match &children {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        // document has 2 direct children: h1 and p1
        assert_eq!(sel.len(), 2);
    }

    #[test]
    fn ned_set_text_mutates_store() {
        let store = make_store_with_tree();
        let root = setup(store.clone());
        let all = call(&root, "ned/all", vec![]).unwrap();
        let text_kw = Value::Keyword { namespace: None, name: "text".to_string() };
        let texts_sel = call(&root, "ned/filter", vec![all, text_kw]).unwrap();
        let new_text = Value::Text("Changed".to_string());
        let result = call(&root, "ned/set-text", vec![texts_sel, new_text]).unwrap();
        // result is a selection of modified nodes
        let modified = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(modified.len(), 2); // both text nodes modified
    }

    #[test]
    fn ned_search_replace_mutates_text_nodes() {
        let store = make_store_with_tree();
        let root = setup(store.clone());
        // Select all text nodes (Hello, World) and replace "o" with "0"
        let all = call(&root, "ned/all", vec![]).unwrap();
        let text_kw = Value::Keyword { namespace: None, name: "text".to_string() };
        let texts_sel = call(&root, "ned/filter", vec![all, text_kw]).unwrap();
        let search = Value::Text("o".to_string());
        let replace = Value::Text("0".to_string());
        let result = call(&root, "ned/search-replace", vec![texts_sel, search, replace]).unwrap();
        // result is the same selection (both nodes)
        let modified = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(modified.len(), 2);
        // Verify text content in store
        let st = store.read().unwrap();
        let all_texts: Vec<_> = modified.iter()
            .filter_map(|id| if let Some(Node::Text(s)) = st.get(id) { Some(s.clone()) } else { None })
            .collect();
        assert!(all_texts.contains(&"Hell0".to_string()), "expected Hell0");
        assert!(all_texts.contains(&"W0rld".to_string()), "expected W0rld");
    }

    #[test]
    fn ned_search_replace_empty_search_is_noop() {
        let store = make_store_with_tree();
        let root = setup(store.clone());
        let all = call(&root, "ned/all", vec![]).unwrap();
        let text_kw = Value::Keyword { namespace: None, name: "text".to_string() };
        let texts_sel = call(&root, "ned/filter", vec![all, text_kw]).unwrap();
        let search = Value::Text("".to_string());
        let replace = Value::Text("X".to_string());
        let result = call(&root, "ned/search-replace", vec![texts_sel, search, replace]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        // Selection returned unchanged
        assert_eq!(sel.len(), 2);
        // Text content is unchanged
        let st = store.read().unwrap();
        let texts: Vec<_> = sel.iter()
            .filter_map(|id| if let Some(Node::Text(s)) = st.get(id) { Some(s.clone()) } else { None })
            .collect();
        assert!(texts.contains(&"Hello".to_string()), "Hello should be unchanged");
        assert!(texts.contains(&"World".to_string()), "World should be unchanged");
    }

    #[test]
    fn ned_search_replace_requires_three_args() {
        let store = make_store_with_tree();
        let root = setup(store);
        let all = call(&root, "ned/all", vec![]).unwrap();
        let text_kw = Value::Keyword { namespace: None, name: "text".to_string() };
        let texts_sel = call(&root, "ned/filter", vec![all, text_kw]).unwrap();
        // Only 2 args — should error
        let err = call(&root, "ned/search-replace", vec![texts_sel, Value::Text("x".to_string())]);
        assert!(err.is_err(), "expected error with too few args");
    }

    #[test]
    fn ned_union_combines_selections() {
        let store = make_store_with_tree();
        let root = setup(store);
        let roots_sel = call(&root, "ned/roots", vec![]).unwrap();
        let all_sel = call(&root, "ned/all", vec![]).unwrap();
        let union = call(&root, "ned/union", vec![roots_sel, all_sel]).unwrap();
        // union of roots and all should equal all
        let all2 = call(&root, "ned/all", vec![]).unwrap();
        let count_all = match call(&root, "ned/count", vec![all2]).unwrap() {
            Value::Integer(n) => n,
            _ => panic!("expected integer"),
        };
        let count_union = match call(&root, "ned/count", vec![union]).unwrap() {
            Value::Integer(n) => n,
            _ => panic!("expected integer"),
        };
        assert_eq!(count_all, count_union);
    }

    #[test]
    fn ned_attr_of_returns_attribute_values() {
        let store = make_store_with_tree();
        let root = setup(store);
        // Get all headings, then extract their "level" attribute
        let all = call(&root, "ned/all", vec![]).unwrap();
        let heading_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let heading_name = Value::Text("heading".to_string());
        let headings = call(&root, "ned/filter", vec![all, heading_kw, heading_name]).unwrap();
        let attr_name = Value::Text("level".to_string());
        let levels = call(&root, "ned/attr-of", vec![headings, attr_name]).unwrap();
        match levels {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], Value::Integer(1)));
            }
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ned_attr_of_returns_empty_for_nodes_without_attr() {
        let store = make_store_with_tree();
        let root = setup(store);
        // Paragraphs have no "level" attribute — result should be empty list
        let all = call(&root, "ned/all", vec![]).unwrap();
        let para_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let para_name = Value::Text("paragraph".to_string());
        let paras = call(&root, "ned/filter", vec![all, para_kw, para_name]).unwrap();
        let attr_name = Value::Text("level".to_string());
        let levels = call(&root, "ned/attr-of", vec![paras, attr_name]).unwrap();
        match levels {
            Value::List(items) => assert_eq!(items.len(), 0),
            _ => panic!("expected List"),
        }
    }

    #[test]
    fn ned_attr_of_accepts_keyword_name() {
        let store = make_store_with_tree();
        let root = setup(store);
        let all = call(&root, "ned/all", vec![]).unwrap();
        let heading_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let heading_name = Value::Text("heading".to_string());
        let headings = call(&root, "ned/filter", vec![all, heading_kw, heading_name]).unwrap();
        // Use keyword as attribute name
        let attr_kw = Value::Keyword { namespace: None, name: "level".to_string() };
        let levels = call(&root, "ned/attr-of", vec![headings, attr_kw]).unwrap();
        match levels {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(items[0], Value::Integer(1)));
            }
            _ => panic!("expected List"),
        }
    }

    // ── NodeTree constructor + mutation binding tests ─────────────────────────

    #[test]
    fn mk_text_returns_opaque_nodetree() {
        let store = make_store_with_tree();
        let root = setup(store);
        let result = call(&root, "ned/mk-text", vec![Value::Text("hello".to_string())]).unwrap();
        let tree = match &result {
            Value::Opaque(a) => a.downcast_ref::<NodeTree>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert!(matches!(tree, NodeTree::Text(s) if s == "hello"));
    }

    #[test]
    fn mk_element_with_child_and_attr_composes() {
        let store = make_store_with_tree();
        let root = setup(store);
        // Build: (ned/mk-element "p") -> with-attr "class" "foo" -> with-child (mk-text "hi")
        let elem = call(&root, "ned/mk-element", vec![Value::Text("p".to_string())]).unwrap();
        let with_attr = call(
            &root,
            "ned/with-attr",
            vec![elem, Value::Text("class".to_string()), Value::Text("foo".to_string())],
        )
        .unwrap();
        let text_tree = call(&root, "ned/mk-text", vec![Value::Text("hi".to_string())]).unwrap();
        let composed = call(&root, "ned/with-child", vec![with_attr, text_tree]).unwrap();
        let tree = match &composed {
            Value::Opaque(a) => a.downcast_ref::<NodeTree>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        match tree {
            NodeTree::Element { name, attrs, children } => {
                assert_eq!(name, "p");
                assert_eq!(attrs, vec![("class".to_string(), "foo".to_string())]);
                assert_eq!(children.len(), 1);
                assert!(matches!(&children[0], NodeTree::Text(s) if s == "hi"));
            }
            _ => panic!("expected Element"),
        }
    }

    #[test]
    fn insert_child_wires_materialized_subtree() {
        let store = make_store_with_tree();
        let root_env = setup(store.clone());
        // Select all elements of kind "heading"
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let heading_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let heading_name = Value::Text("heading".to_string());
        let headings = call(&root_env, "ned/filter", vec![all, heading_kw, heading_name]).unwrap();
        // Build a span tree and insert as child
        let span_tree = call(&root_env, "ned/mk-element", vec![Value::Text("span".to_string())]).unwrap();
        let result = call(&root_env, "ned/insert-child", vec![headings, span_tree]).unwrap();
        let created = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert_eq!(created.len(), 1);
        // Verify the span is in the store and is a child of the heading
        let st = store.read().unwrap();
        let span_id = created.iter().next().unwrap();
        match st.get(span_id) {
            Some(Node::Element(n)) => assert_eq!(st.resolve_name(*n), "span"),
            other => panic!("expected Element(span), got {other:?}"),
        }
    }

    #[test]
    fn replace_swaps_subtree_via_binding() {
        let store = make_store_with_tree();
        let root_env = setup(store.clone());
        // Use ned/filter :kind heading then ned/children to isolate the heading's text child
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let heading_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let heading_name = Value::Text("heading".to_string());
        let headings = call(&root_env, "ned/filter", vec![all, heading_kw, heading_name]).unwrap();
        let heading_children = call(&root_env, "ned/children", vec![headings]).unwrap();
        // Build replacement list: [(ned/mk-text "new")]
        let new_text_tree = call(&root_env, "ned/mk-text", vec![Value::Text("new".to_string())]).unwrap();
        let trees_list = Value::List(vec![new_text_tree]);
        let _result = call(&root_env, "ned/replace", vec![heading_children, trees_list]).unwrap();
        // Verify the heading now has a child with text "new"
        let st = store.read().unwrap();
        let all_sel = {
            let all_v = call(&root_env, "ned/all", vec![]).unwrap();
            match &all_v {
                Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
                _ => panic!(),
            }
        };
        // Find text nodes: look for Node::Text("new") in store
        let has_new = all_sel.iter().any(|id| {
            matches!(st.get(id), Some(Node::Text(s)) if s == "new")
        });
        assert!(has_new, "expected 'new' text node in store");
        // The old "Hello" text should be gone
        let has_hello = all_sel.iter().any(|id| {
            matches!(st.get(id), Some(Node::Text(s)) if s == "Hello")
        });
        assert!(!has_hello, "expected 'Hello' to be removed");
    }

    // ── ned/nth-child tests ───────────────────────────────────────────────────

    #[test]
    fn ned_nth_child_picks_by_insertion_order() {
        let store = make_store_with_tree();
        let root_env = setup(store.clone());
        // The tree has doc > [h1, p1]. h1 is first child, p1 is second.
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let doc_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let doc_name = Value::Text("document".to_string());
        let docs = call(&root_env, "ned/filter", vec![all, doc_kw, doc_name]).unwrap();
        // Get 0th child of doc — should be h1 (a heading)
        let first = call(&root_env, "ned/nth-child", vec![docs, Value::Integer(0)]).unwrap();
        let sel = match &first {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(sel.len(), 1);
        let st = store.read().unwrap();
        let id = sel.iter().next().unwrap();
        assert!(matches!(st.get(id), Some(Node::Element(n)) if st.resolve_name(*n) == "heading"));
    }

    #[test]
    fn ned_nth_child_out_of_range_returns_empty() {
        let store = make_store_with_tree();
        let root_env = setup(store);
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let doc_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let doc_name = Value::Text("document".to_string());
        let docs = call(&root_env, "ned/filter", vec![all, doc_kw, doc_name]).unwrap();
        let result = call(&root_env, "ned/nth-child", vec![docs, Value::Integer(99)]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert!(sel.is_empty());
    }

    // ── ned prelude helpers: doc-by-path, slot, body-at ───────────────────────

    fn setup_with_prelude(store: Arc<RwLock<NodeStore>>) -> RootEnv {
        let root = RootEnv::new();
        crate::init_root(&root).expect("core init failed");
        register_ned_builtins(&root, store);
        crate::load_ned_prelude(&root).expect("ned prelude load failed");
        root
    }

    fn make_doc_store() -> Arc<RwLock<NodeStore>> {
        use content::{ContentElement, Document, DocumentSlot};
        use schema::{HeadingLevel, SlotName, Span, Spanned};
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

        let mut store = NodeStore::new();

        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::Heading {
                            level: HeadingLevel::new(1).unwrap(),
                            text: "My Title".to_string(),
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                }
            ],
            body: im::vector![
                Spanned {
                    node: ContentElement::Paragraph { text: "First".to_string() },
                    span: Span { start: 0, end: 0 },
                },
                Spanned {
                    node: ContentElement::Paragraph { text: "Second".to_string() },
                    span: Span { start: 0, end: 0 },
                },
                Spanned {
                    node: ContentElement::Paragraph { text: "Third".to_string() },
                    span: Span { start: 0, end: 0 },
                },
            ],
            has_separator: false,
            separator_span: None,
        };

        let meta = DocumentMeta {
            url: "/post/foo".to_string(),
            stem: "post".to_string(),
            file: "content/post/foo.md".to_string(),
            page_kind: "item".to_string(),
        };

        document_to_store(&doc, &mut store, Some(&meta));
        Arc::new(RwLock::new(store))
    }

    #[test]
    fn doc_by_path_finds_matching_document() {
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);
        let result = crate::eval_str_with_root(
            r#"(ned/count (ned/doc-by-path "content/post/foo.md"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(result, Value::Integer(1)));
    }

    #[test]
    fn doc_by_path_returns_empty_for_missing() {
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);
        let result = crate::eval_str_with_root(
            r#"(ned/count (ned/doc-by-path "content/post/missing.md"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(result, Value::Integer(0)));
    }

    #[test]
    fn slot_traverses_to_named_slot() {
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);
        // The slot element should have kind "slot" and attr name="title"
        let count_result = crate::eval_str_with_root(
            r#"(ned/count (ned/slot (ned/doc-by-path "content/post/foo.md") "title"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(count_result, Value::Integer(1)), "expected 1 slot named title");

        // Verify it is kind "slot"
        let kind_result = crate::eval_str_with_root(
            r#"(ned/kind-of (ned/slot (ned/doc-by-path "content/post/foo.md") "title"))"#,
            &root_env,
        )
        .unwrap();
        match kind_result {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], Value::Text(s) if s == "slot"));
            }
            _ => panic!("expected List from ned/kind-of"),
        }
    }

    #[test]
    fn slot_returns_empty_for_missing_name() {
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);
        let result = crate::eval_str_with_root(
            r#"(ned/count (ned/slot (ned/doc-by-path "content/post/foo.md") "nonexistent"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(result, Value::Integer(0)));
    }

    #[test]
    fn body_at_picks_idx() {
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);
        // body has 3 paragraphs: First, Second, Third at indices 0, 1, 2
        // Index 1 should give us the second paragraph ("Second")
        let count_result = crate::eval_str_with_root(
            r#"(ned/count (ned/body-at (ned/doc-by-path "content/post/foo.md") 1))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(count_result, Value::Integer(1)));

        // Verify it is kind "paragraph"
        let kind_result = crate::eval_str_with_root(
            r#"(ned/kind-of (ned/body-at (ned/doc-by-path "content/post/foo.md") 1))"#,
            &root_env,
        )
        .unwrap();
        match kind_result {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], Value::Text(s) if s == "paragraph"));
            }
            _ => panic!("expected List from ned/kind-of"),
        }
    }

    #[test]
    fn body_at_out_of_range_returns_empty() {
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);
        let result = crate::eval_str_with_root(
            r#"(ned/count (ned/body-at (ned/doc-by-path "content/post/foo.md") 10))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(result, Value::Integer(0)));
    }

    #[test]
    fn body_at_zero_on_empty_body_returns_empty() {
        use content::Document;
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

        let mut raw_store = NodeStore::new();
        let doc = Document {
            preamble: im::vector![],
            body: im::vector![],
            has_separator: false,
            separator_span: None,
        };
        let meta = DocumentMeta {
            url: "/post/empty".to_string(),
            stem: "post".to_string(),
            file: "content/post/empty.md".to_string(),
            page_kind: "item".to_string(),
        };
        document_to_store(&doc, &mut raw_store, Some(&meta));
        let store = Arc::new(RwLock::new(raw_store));

        let root_env = setup_with_prelude(store);
        let result = crate::eval_str_with_root(
            r#"(ned/count (ned/body-at (ned/doc-by-path "content/post/empty.md") 0))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(result, Value::Integer(0)));
    }

    // ── ned/parse-grammar and ned/parse-body tests ────────────────────────────

    const EMPTY_SCHEMA_SRC: &str = "preamble: []\n";

    fn make_grammar_value(root_env: &RootEnv) -> Value {
        call(root_env, "ned/parse-grammar", vec![Value::Text(EMPTY_SCHEMA_SRC.to_string())]).unwrap()
    }

    #[test]
    fn parse_grammar_returns_opaque_grammar() {
        let store = make_store_with_tree();
        let root_env = setup(store);
        let result = call(&root_env, "ned/parse-grammar", vec![Value::Text(EMPTY_SCHEMA_SRC.to_string())]).unwrap();
        match &result {
            Value::Opaque(a) => {
                assert!(a.downcast_ref::<Arc<Grammar>>().is_some(), "expected Arc<Grammar> opaque");
            }
            _ => panic!("expected Opaque"),
        }
    }

    #[test]
    fn parse_grammar_accepts_any_text_schema_is_permissive() {
        // The schema parser is a custom markdown-style parser (not YAML).
        // It is very permissive and does not fail on unrecognised input;
        // unknown lines are silently skipped, producing an empty Grammar.
        // This test documents that behaviour.
        let store = make_store_with_tree();
        let root_env = setup(store);
        let result = call(&root_env, "ned/parse-grammar", vec![Value::Text(":::unrecognised:::".to_string())]);
        assert!(result.is_ok(), "schema parser should accept any text and produce an empty Grammar");
    }

    #[test]
    fn parse_body_ingests_paragraph() {
        let store = make_store_with_tree();
        let root_env = setup(store.clone());
        let grammar = make_grammar_value(&root_env);
        let result = call(
            &root_env,
            "ned/parse-body",
            vec![Value::Text("Hello world\n".to_string()), grammar],
        )
        .unwrap();
        let trees = match result {
            Value::List(items) => items,
            _ => panic!("expected List"),
        };
        assert_eq!(trees.len(), 1, "expected exactly one element");

        let tree = extract_node_tree(&trees[0]).expect("expected NodeTree");
        let st = store.read().unwrap();
        match tree {
            NodeTree::Existing(id) => {
                match st.get(id) {
                    Some(Node::Element(name)) => {
                        assert_eq!(st.resolve_name(*name), "paragraph");
                    }
                    other => panic!("expected Element(paragraph), got {other:?}"),
                }
                let children = st.children(id);
                assert!(
                    children.iter().any(|&cid| matches!(st.get(cid), Some(Node::Text(s)) if s == "Hello world")),
                    "expected 'Hello world' text child"
                );
            }
            _ => panic!("expected NodeTree::Existing"),
        }
    }

    #[test]
    fn parse_body_ingests_multi_element() {
        let store = make_store_with_tree();
        let root_env = setup(store);
        let grammar = make_grammar_value(&root_env);
        let result = call(
            &root_env,
            "ned/parse-body",
            vec![Value::Text("# Heading\n\nPara\n".to_string()), grammar],
        )
        .unwrap();
        let trees = match result {
            Value::List(items) => items,
            _ => panic!("expected List"),
        };
        assert_eq!(trees.len(), 2, "expected heading + paragraph");
    }

    #[test]
    fn replace_with_parse_body_swaps_body_element() {
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
        use content::{ContentElement, Document, DocumentSlot};
        use schema::{HeadingLevel, SlotName, Span, Spanned};

        let mut raw_store = NodeStore::new();
        let doc = Document {
            preamble: im::vector![
                DocumentSlot {
                    name: SlotName::new("title"),
                    elements: im::vector![Spanned {
                        node: ContentElement::Heading {
                            level: HeadingLevel::new(1).unwrap(),
                            text: "Title".to_string(),
                        },
                        span: Span { start: 0, end: 0 },
                    }],
                }
            ],
            body: im::vector![
                Spanned {
                    node: ContentElement::Paragraph { text: "Old content".to_string() },
                    span: Span { start: 0, end: 0 },
                },
            ],
            has_separator: false,
            separator_span: None,
        };
        let meta = DocumentMeta {
            url: "/post/replace-test".to_string(),
            stem: "post".to_string(),
            file: "content/post/replace-test.md".to_string(),
            page_kind: "item".to_string(),
        };
        document_to_store(&doc, &mut raw_store, Some(&meta));
        let store = Arc::new(RwLock::new(raw_store));

        let root_env = setup_with_prelude(store.clone());
        let grammar = make_grammar_value(&root_env);

        let doc_sel = crate::eval_str_with_root(
            r#"(ned/doc-by-path "content/post/replace-test.md")"#,
            &root_env,
        ).unwrap();
        let body_elem = call(&root_env, "ned/body-at", vec![doc_sel, Value::Integer(0)]).unwrap();

        let new_trees = call(
            &root_env,
            "ned/parse-body",
            vec![Value::Text("# New heading\n".to_string()), grammar],
        ).unwrap();

        let result = call(&root_env, "ned/replace", vec![body_elem, new_trees]).unwrap();
        let inserted = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert_eq!(inserted.len(), 1, "expected one inserted node");

        let st = store.read().unwrap();
        let inserted_id = inserted.iter().next().unwrap();
        match st.get(inserted_id) {
            Some(Node::Element(name)) => {
                assert_eq!(st.resolve_name(*name), "heading", "inserted node should be a heading");
            }
            other => panic!("expected Element(heading), got {other:?}"),
        }
    }

    #[test]
    fn parse_body_valid_input_always_succeeds() {
        // pulldown_cmark is very permissive; document that any valid markdown succeeds
        let store = make_store_with_tree();
        let root_env = setup(store);
        let grammar = make_grammar_value(&root_env);
        let result = call(
            &root_env,
            "ned/parse-body",
            vec![Value::Text("Normal text\n".to_string()), grammar],
        );
        assert!(result.is_ok(), "valid input should succeed: {result:?}");
    }

    // ── ned/source-docs-of tests ─────────────────────────────────────────────

    /// Build a store with two distinct documents (doc A and doc B), each with a body paragraph.
    /// Returns the store plus the IDs of: doc_a_root, doc_a_para, doc_b_root, doc_b_para.
    fn make_two_doc_store() -> (Arc<RwLock<NodeStore>>, node_store::NodeId, node_store::NodeId, node_store::NodeId, node_store::NodeId) {
        use node_store::NodeStore;
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
        use content::{ContentElement, Document};
        use schema::{Span, Spanned};

        let mut store = NodeStore::new();

        let make_doc = |store: &mut NodeStore, file: &str, url: &str, text: &str| {
            let doc = Document {
                preamble: im::vector![],
                body: im::vector![Spanned {
                    node: ContentElement::Paragraph { text: text.to_string() },
                    span: Span { start: 0, end: 0 },
                }],
                has_separator: false,
                separator_span: None,
            };
            let meta = DocumentMeta {
                url: url.to_string(),
                stem: "post".to_string(),
                file: file.to_string(),
                page_kind: "item".to_string(),
            };
            document_to_store(&doc, store, Some(&meta))
        };

        let doc_a_root = make_doc(&mut store, "content/a.md", "/a", "Para A");
        let doc_b_root = make_doc(&mut store, "content/b.md", "/b", "Para B");

        // Find the paragraph children of each doc via the body element
        let find_para = |store: &NodeStore, doc_root: node_store::NodeId| -> node_store::NodeId {
            // doc_root > body > paragraph
            let body = store.children(doc_root)
                .iter()
                .copied()
                .find(|&id| matches!(store.get(id), Some(Node::Element(n)) if store.resolve_name(*n) == "body"))
                .expect("body child");
            store.children(body)
                .iter()
                .copied()
                .find(|&id| matches!(store.get(id), Some(Node::Element(n)) if store.resolve_name(*n) == "paragraph"))
                .expect("paragraph child")
        };

        let doc_a_para = find_para(&store, doc_a_root);
        let doc_b_para = find_para(&store, doc_b_root);

        (Arc::new(RwLock::new(store)), doc_a_root, doc_a_para, doc_b_root, doc_b_para)
    }

    #[test]
    fn source_docs_of_single_node_returns_containing_doc() {
        let (store, doc_a_root, doc_a_para, _doc_b_root, _doc_b_para) = make_two_doc_store();
        let root_env = setup_with_prelude(store);

        // Select the paragraph from doc A; source-docs-of should return doc A's root.
        let para_sel = wrap_selection(Selection::single(doc_a_para));
        let result_val = call(&root_env, "ned/source-docs-of", vec![para_sel]).unwrap();
        let result = match &result_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert_eq!(result.len(), 1, "expected exactly one doc root");
        assert!(result.contains(doc_a_root), "expected doc_a_root in result");
    }

    #[test]
    fn source_docs_of_multiple_docs_returns_multiple_roots() {
        let (store, doc_a_root, doc_a_para, doc_b_root, doc_b_para) = make_two_doc_store();
        let root_env = setup_with_prelude(store);

        // Select one node from each document; result should contain both roots.
        let combined = Selection::from_ids([doc_a_para, doc_b_para]);
        let result_val = call(&root_env, "ned/source-docs-of", vec![wrap_selection(combined)]).unwrap();
        let result = match &result_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert_eq!(result.len(), 2, "expected both doc roots");
        assert!(result.contains(doc_a_root), "expected doc_a_root");
        assert!(result.contains(doc_b_root), "expected doc_b_root");
    }

    #[test]
    fn source_docs_of_doc_root_returns_itself() {
        let (store, doc_a_root, _doc_a_para, _doc_b_root, _doc_b_para) = make_two_doc_store();
        let root_env = setup_with_prelude(store);

        // Selecting a document root directly: source-docs-of returns that same root.
        let doc_sel = wrap_selection(Selection::single(doc_a_root));
        let result_val = call(&root_env, "ned/source-docs-of", vec![doc_sel]).unwrap();
        let result = match &result_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert_eq!(result.len(), 1, "expected exactly one doc root");
        assert!(result.contains(doc_a_root), "expected doc_a_root itself");
    }

    #[test]
    fn source_docs_of_empty_selection_returns_empty() {
        let (store, _doc_a_root, _doc_a_para, _doc_b_root, _doc_b_para) = make_two_doc_store();
        let root_env = setup_with_prelude(store);

        let empty_sel = wrap_selection(Selection::new());
        let result_val = call(&root_env, "ned/source-docs-of", vec![empty_sel]).unwrap();
        let result = match &result_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert!(result.is_empty(), "expected empty result for empty input");
    }

    #[test]
    fn source_docs_of_dedups_same_document() {
        let (store, doc_a_root, doc_a_para, _doc_b_root, _doc_b_para) = make_two_doc_store();
        // We also need another node from doc A — get the body element itself
        let body_id = {
            let st = store.read().unwrap();
            st.children(doc_a_root)
                .iter()
                .copied()
                .find(|&id| matches!(st.get(id), Some(Node::Element(n)) if st.resolve_name(*n) == "body"))
                .expect("body")
        };

        let root_env = setup_with_prelude(store);

        // Two nodes from the same document (body and one of its children) — should give one root.
        let combined = Selection::from_ids([doc_a_para, body_id]);
        let result_val = call(&root_env, "ned/source-docs-of", vec![wrap_selection(combined)]).unwrap();
        let result = match &result_val {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque Selection"),
        };
        assert_eq!(result.len(), 1, "expected one root even for two nodes from the same doc");
        assert!(result.contains(doc_a_root));
    }

    // ── Fixtures for new Phase-1 tests ────────────────────────────────────────

    /// Store with: doc > [heading("Hello"), paragraph("World")]
    /// heading has text child "Hello"; paragraph has text child "World".
    fn make_store_with_heading_and_para() -> Arc<RwLock<NodeStore>> {
        let mut store = NodeStore::new();
        let doc_name = store.intern("document");
        let heading_name = store.intern("heading");
        let para_name = store.intern("paragraph");

        let root = store.add_node(Node::Element(doc_name));

        let h = store.add_node(Node::Element(heading_name));
        let t_h = store.add_node(Node::Text("Hello".to_string()));
        store.add_edge(h, Edge::Child(t_h));

        let p = store.add_node(Node::Element(para_name));
        let t_p = store.add_node(Node::Text("World".to_string()));
        store.add_edge(p, Edge::Child(t_p));

        store.add_edge(root, Edge::Child(h));
        store.add_edge(root, Edge::Child(p));

        Arc::new(RwLock::new(store))
    }

    // ── ned/nth-of-kind binding tests ─────────────────────────────────────────

    #[test]
    fn ned_nth_of_kind_basic() {
        let store = make_store_with_tree();
        let root_env = setup(store.clone());
        // make_store_with_tree has doc > [heading, paragraph]
        // Get doc's children (heading + paragraph), then nth-of-kind "paragraph" 0 → the paragraph.
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let doc_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let doc_name = Value::Text("document".to_string());
        let docs = call(&root_env, "ned/filter", vec![all, doc_kw, doc_name]).unwrap();
        let children = call(&root_env, "ned/children", vec![docs.clone()]).unwrap();

        let result = call(&root_env, "ned/nth-of-kind", vec![
            children.clone(),
            Value::Text("paragraph".to_string()),
            Value::Integer(0),
        ]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(sel.len(), 1);
        let st = store.read().unwrap();
        let id = sel.iter().next().unwrap();
        assert!(matches!(st.get(id), Some(Node::Element(n)) if st.resolve_name(*n) == "paragraph"));

        // Out of range — index 1 beyond the single paragraph
        let empty_result = call(&root_env, "ned/nth-of-kind", vec![
            children,
            Value::Text("paragraph".to_string()),
            Value::Integer(1),
        ]).unwrap();
        let empty_sel = match &empty_result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert!(empty_sel.is_empty());
    }

    // ── ned/following-siblings and ned/preceding-siblings binding tests ────────

    #[test]
    fn ned_following_siblings_basic() {
        // doc > [heading, paragraph]: following-siblings of heading = [paragraph]
        let store = make_store_with_heading_and_para();
        let root_env = setup(store.clone());

        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let heading_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let heading_name = Value::Text("heading".to_string());
        let headings = call(&root_env, "ned/filter", vec![all, heading_kw, heading_name]).unwrap();

        let result = call(&root_env, "ned/following-siblings", vec![headings]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(sel.len(), 1);
        let st = store.read().unwrap();
        let id = sel.iter().next().unwrap();
        assert!(matches!(st.get(id), Some(Node::Element(n)) if st.resolve_name(*n) == "paragraph"));
    }

    #[test]
    fn ned_preceding_siblings_basic() {
        // doc > [heading, paragraph]: preceding-siblings of paragraph = [heading]
        let store = make_store_with_heading_and_para();
        let root_env = setup(store.clone());

        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let para_kw = Value::Keyword { namespace: None, name: "kind".to_string() };
        let para_name = Value::Text("paragraph".to_string());
        let paras = call(&root_env, "ned/filter", vec![all, para_kw, para_name]).unwrap();

        let result = call(&root_env, "ned/preceding-siblings", vec![paras]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert_eq!(sel.len(), 1);
        let st = store.read().unwrap();
        let id = sel.iter().next().unwrap();
        assert!(matches!(st.get(id), Some(Node::Element(n)) if st.resolve_name(*n) == "heading"));
    }

    // ── ned/filter :text-contains and :text-equals binding tests ─────────────

    #[test]
    fn ned_filter_text_contains() {
        let store = make_store_with_heading_and_para();
        let root_env = setup(store);
        // heading has text "Hello"; filter :text-contains "ell"
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let kw = Value::Keyword { namespace: None, name: "text-contains".to_string() };
        let needle = Value::Text("ell".to_string());
        let result = call(&root_env, "ned/filter", vec![all, kw, needle]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        // At minimum the heading element should be included (its text descendant is "Hello")
        assert!(sel.len() >= 1, "expected at least one match");
    }

    #[test]
    fn ned_filter_text_equals() {
        let store = make_store_with_heading_and_para();
        let root_env = setup(store);
        // paragraph has text "World"; filter :text-equals "World"
        let all = call(&root_env, "ned/all", vec![]).unwrap();
        let kw = Value::Keyword { namespace: None, name: "text-equals".to_string() };
        let value = Value::Text("World".to_string());
        let result = call(&root_env, "ned/filter", vec![all, kw, value]).unwrap();
        let sel = match &result {
            Value::Opaque(a) => a.downcast_ref::<Selection>().cloned().unwrap(),
            _ => panic!("expected Opaque"),
        };
        assert!(sel.len() >= 1, "expected at least one match");
    }

    // ── Prelude helper: ned/nth-after ─────────────────────────────────────────

    #[test]
    fn nth_after_prelude_helper() {
        // Use make_doc_store: doc has body with [paragraph("First"), paragraph("Second"), paragraph("Third")]
        // (ned/body-at doc 0) is First paragraph; its following siblings contain Second and Third.
        // (ned/nth-after (ned/body-at doc 0) "paragraph" 0) should be the Second paragraph.
        let store = make_doc_store();
        let root_env = setup_with_prelude(store);

        // Count should be 1
        let count_result = crate::eval_str_with_root(
            r#"(ned/count (ned/nth-after (ned/body-at (ned/doc-by-path "content/post/foo.md") 0) "paragraph" 0))"#,
            &root_env,
        ).unwrap();
        assert!(matches!(count_result, Value::Integer(1)), "expected 1 element");

        // Text content should be "Second" (first following-paragraph after the first body element)
        let text_result = crate::eval_str_with_root(
            r#"(ned/text-of (ned/filter (ned/descendants (ned/nth-after (ned/body-at (ned/doc-by-path "content/post/foo.md") 0) "paragraph" 0)) :text))"#,
            &root_env,
        ).unwrap();
        match text_result {
            Value::List(items) => {
                assert_eq!(items.len(), 1);
                assert!(matches!(&items[0], Value::Text(s) if s == "Second"),
                    "expected 'Second', got {items:?}");
            }
            _ => panic!("expected List"),
        }
    }
}
