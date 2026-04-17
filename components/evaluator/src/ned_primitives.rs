use std::sync::{Arc, RwLock};
use template::Value;
use node_store::{Node, NodeStore};
use ned::Selection;
use ned::selection;
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
    reg(root, "ned/delete", "(ned/delete sel)", "Delete selected nodes and their descendants.", move |args| {
        if args.is_empty() { return Err("ned/delete requires a selection".into()); }
        let sel = extract_selection(&args[0])?;
        let mut st = s.write().map_err(|e| e.to_string())?;
        let result = ned::mutation::delete(&mut st, &sel);
        Ok(wrap_selection(result))
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
}
