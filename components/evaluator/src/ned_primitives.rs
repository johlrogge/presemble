use std::sync::{Arc, Mutex, RwLock};
use content_store::edit::{CommentPlace, NedMutation, Plan, PlannedMutation};
use template::Value;
use node_store::{Node, NodeId, NodeStore};
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

/// Extract a non-negative integer index argument. Negative or non-integer → error.
fn extract_non_negative_index(fn_name: &str, val: &Value) -> Result<usize, String> {
    match val {
        Value::Integer(n) if *n < 0 => Err(format!("{fn_name}: idx must be non-negative")),
        Value::Integer(n) => Ok(*n as usize),
        _ => Err(format!("{fn_name}: idx must be an integer")),
    }
}

// ---------------------------------------------------------------------------
// Registration entry point
// ---------------------------------------------------------------------------

/// The mutations a program asked for, in evaluation order (select-then-apply,
/// ADR-059 stage 2e).
///
/// The planning builtins record into it instead of writing; the caller takes
/// the [`Plan`] after evaluation and runs it with `content_store::edit::execute`.
/// If evaluation fails, the caller drops the recorder and nothing is applied.
#[derive(Debug, Clone, Default)]
pub struct PlanRecorder(Arc<Mutex<Vec<PlannedMutation>>>);

impl PlanRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    fn record(&self, step: PlannedMutation) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).push(step);
    }

    /// The recorded steps, leaving the recorder empty.
    pub fn take(&self) -> Plan {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|e| e.into_inner()))
            .into_iter()
            .collect()
    }
}

/// Names of every builtin registered by [`register_ned_mutation_builtins`].
///
/// These are the builtins bound only in program roots, never in the
/// query-only root that evaluates addresses (a root populated only by
/// [`register_ned_query_builtins`] binds none of them).
///
/// All but `ned/parse-body` record plan steps; they write nothing themselves.
/// `ned/parse-body` is pure: it is listed only because it has no use in an
/// address. Whether it belongs in the query root is decided in 2e-2.
pub const NED_MUTATING_BUILTINS: &[&str] = &[
    "ned/set-text",
    "ned/search-replace",
    "ned/delete",
    "ned/insert-child",
    "ned/insert-before",
    "ned/insert-after",
    "ned/replace",
    "ned/set-comments-before",
    "ned/set-end-comments",
    "ned/parse-body",
];

/// Register all NED primitives (query and mutation) as core evaluator builtins.
/// The store is shared via Arc<RwLock<NodeStore>>; the mutating builtins
/// record into `recorder` and never touch it.
///
/// Equivalent to calling [`register_ned_query_builtins`] followed by
/// [`register_ned_mutation_builtins`].
pub fn register_ned_builtins(root: &RootEnv, store: Arc<RwLock<NodeStore>>, recorder: &PlanRecorder) {
    register_ned_query_builtins(root, store);
    register_ned_mutation_builtins(root, recorder);
}

/// Register the read-only NED primitives: selection constructors, traversal,
/// filtering, inspection, set operations, attribute and positional access,
/// pure NodeTree constructors, and `ned/parse-grammar`.
///
/// None of these write to the store. None of the names in
/// [`NED_MUTATING_BUILTINS`] are bound.
pub fn register_ned_query_builtins(root: &RootEnv, store: Arc<RwLock<NodeStore>>) {
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
                    // Headings hold inline nodes; this compares their plain
                    // text, as rendered (ADR-059 §8).
                    "heading-text-equals" => {
                        let value = match args.get(2) {
                            Some(Value::Text(s)) => s.clone(),
                            _ => return Err("ned/filter :heading-text-equals requires a string".into()),
                        };
                        let target = normalize_text(&value);
                        let matches = move |store: &NodeStore, id: NodeId| {
                            content_store::heading_plain_text(store, id)
                                .is_some_and(|plain| normalize_text(&plain) == target)
                        };
                        Ok(wrap_selection(sel.filter(&st, matches)))
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

    let s = store.clone();
    reg(root, "ned/nth-text", "(ned/nth-text sel idx)", "For each node in sel, the idx-th Text node under it (depth-first, document order). Union over all scope nodes: two scope nodes may give two results. Out of range gives nothing; negative idx is an error.", move |args| {
        if args.len() < 2 { return Err("ned/nth-text requires a selection and an index".into()); }
        let sel = extract_selection(&args[0])?;
        let idx = extract_non_negative_index("ned/nth-text", &args[1])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.nth_text_each(&st, idx)))
    });

    let s = store.clone();
    reg(root, "ned/nth-following-of-kind", "(ned/nth-following-of-kind sel kind idx)", "For each node in sel, its idx-th following sibling element of the given kind (document order). Union over all scope nodes: each anchor resolves separately. Out of range gives nothing; negative idx is an error.", move |args| {
        if args.len() < 3 { return Err("ned/nth-following-of-kind requires a selection, kind name, and index".into()); }
        let sel = extract_selection(&args[0])?;
        let kind = match &args[1] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/nth-following-of-kind: kind name must be a string".into()),
        };
        let idx = extract_non_negative_index("ned/nth-following-of-kind", &args[2])?;
        let st = s.read().map_err(|e| e.to_string())?;
        Ok(wrap_selection(sel.nth_following_of_kind_each(&st, &kind, idx)))
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

    // ── Grammar parsing (pure) ────────────────────────────────────────────────

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
}

/// Register a builtin that records one planned mutation and returns `nil`.
///
/// The nodes a mutation creates do not exist while the program runs, so there
/// is nothing to return: a program that feeds the result of one mutation into
/// another builtin fails loudly ("expected a Selection").
fn reg_planned(
    root: &RootEnv,
    recorder: &PlanRecorder,
    name: &'static str,
    sig: &str,
    doc: &str,
    build: impl Fn(Vec<Value>) -> Result<(Selection, NedMutation), String> + Send + Sync + 'static,
) {
    let recorder = recorder.clone();
    reg(root, name, sig, doc, move |args| {
        let (targets, mutation) = build(args)?;
        recorder.record(PlannedMutation { targets, mutation, origin: name });
        Ok(Value::Absent)
    });
}

/// The arguments of the two comment builtins: a selection and a list of
/// strings.
fn comments_args(name: &str, args: &[Value]) -> Result<(Selection, Vec<String>), String> {
    if args.len() < 2 {
        return Err(format!("{name} requires selection and list of comments"));
    }
    let sel = extract_selection(&args[0])?;
    let comments = match &args[1] {
        Value::List(items) => items
            .iter()
            .map(|item| match item {
                Value::Text(s) => Ok(s.clone()),
                _ => Err(format!("{name}: every comment must be a string")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(format!("{name}: second argument must be a list of strings")),
    };
    Ok((sel, comments))
}

/// `s` with every run of whitespace as one space, trimmed (the comparison
/// `ned/filter :heading-text-equals` makes).
fn normalize_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Register the NED primitives that plan mutations, and `ned/parse-body`. The
/// registered names are exactly [`NED_MUTATING_BUILTINS`].
///
/// The planning builtins write nothing: each records a [`PlannedMutation`] in
/// `recorder` (the selection as it was when the program ran, and the
/// mutation) and returns `nil`. Whether a selection addresses exactly one node
/// of the right kind is decided later, by `content_store::edit::execute`.
pub fn register_ned_mutation_builtins(root: &RootEnv, recorder: &PlanRecorder) {
    // ── Mutations ─────────────────────────────────────────────────────────────

    reg_planned(root, recorder, "ned/set-text", "(ned/set-text sel text)", "Plan: set the text of the one Text node sel addresses.", |args| {
        if args.len() < 2 { return Err("ned/set-text requires selection and text".into()); }
        let sel = extract_selection(&args[0])?;
        let text = match &args[1] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/set-text: second argument must be a string".into()),
        };
        Ok((sel, NedMutation::SetText(text)))
    });

    reg_planned(root, recorder, "ned/search-replace", "(ned/search-replace sel search replace)", "Plan: replace the one occurrence of search with replace in the one Text node sel addresses.", |args| {
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
        Ok((sel, NedMutation::SearchReplace { search, replace }))
    });

    reg_planned(root, recorder, "ned/delete", "(ned/delete sel)", "Plan: delete the one block sel addresses.", |args| {
        if args.is_empty() { return Err("ned/delete requires a selection".into()); }
        Ok((extract_selection(&args[0])?, NedMutation::Delete))
    });

    // ── Tree-based mutation bindings ──────────────────────────────────────────

    reg_planned(root, recorder, "ned/insert-child", "(ned/insert-child sel tree)", "Plan: insert a NodeTree as the last child of the one body or slot sel addresses.", |args| {
        if args.len() < 2 { return Err("ned/insert-child requires selection and tree".into()); }
        let sel = extract_selection(&args[0])?;
        let tree = extract_node_tree(&args[1])?;
        Ok((sel, NedMutation::InsertChild(vec![tree])))
    });

    reg_planned(root, recorder, "ned/insert-before", "(ned/insert-before sel tree)", "Plan: insert a NodeTree before the one block sel addresses.", |args| {
        if args.len() < 2 { return Err("ned/insert-before requires selection and tree".into()); }
        let sel = extract_selection(&args[0])?;
        let tree = extract_node_tree(&args[1])?;
        Ok((sel, NedMutation::InsertBefore(vec![tree])))
    });

    reg_planned(root, recorder, "ned/insert-after", "(ned/insert-after sel tree)", "Plan: insert a NodeTree after the one block sel addresses.", |args| {
        if args.len() < 2 { return Err("ned/insert-after requires selection and tree".into()); }
        let sel = extract_selection(&args[0])?;
        let tree = extract_node_tree(&args[1])?;
        Ok((sel, NedMutation::InsertAfter(vec![tree])))
    });

    reg_planned(root, recorder, "ned/replace", "(ned/replace sel trees)", "Plan: replace the one block sel addresses with a list of NodeTrees.", |args| {
        if args.len() < 2 { return Err("ned/replace requires selection and list of trees".into()); }
        let sel = extract_selection(&args[0])?;
        let trees = extract_node_tree_list(&args[1])?;
        Ok((sel, NedMutation::Replace(trees)))
    });

    reg_planned(root, recorder, "ned/set-comments-before", "(ned/set-comments-before sel comments)", "Plan: replace the comments directly before the one block sel addresses with a list of HTML comment strings.", |args| {
        let (sel, comments) = comments_args("ned/set-comments-before", &args)?;
        Ok((sel, NedMutation::SetComments { place: CommentPlace::Before, comments }))
    });

    reg_planned(root, recorder, "ned/set-end-comments", "(ned/set-end-comments sel comments)", "Plan: replace the comments after the last block of the one container (body, slot, unassigned, blockquote, item) sel addresses with a list of HTML comment strings.", |args| {
        let (sel, comments) = comments_args("ned/set-end-comments", &args)?;
        Ok((sel, NedMutation::SetComments { place: CommentPlace::End, comments }))
    });

    // ── Body-fragment parsing (pure) ──────────────────────────────────────────

    reg(root, "ned/parse-body", "(ned/parse-body md-source grammar)", "Parse a markdown body fragment into a list of NodeTrees. Pure: nothing is written. html blocks, separators and comments are refused.", |args| {
        if args.len() < 2 { return Err("ned/parse-body requires markdown source and grammar".into()); }
        let md_src = match &args[0] {
            Value::Text(s) => s.clone(),
            _ => return Err("ned/parse-body: first argument must be a string".into()),
        };
        let grammar = extract_grammar(&args[1])?;
        let trees = node_store_bridge::parse_body_fragment(&md_src, &grammar)
            .map_err(|e| format!("ned/parse-body: {e}"))?;
        Ok(Value::List(trees.into_iter().map(wrap_node_tree).collect()))
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
        setup_recording(store).0
    }

    /// A root whose planning builtins record into the returned recorder.
    fn setup_recording(store: Arc<RwLock<NodeStore>>) -> (RootEnv, PlanRecorder) {
        let root = RootEnv::new();
        let recorder = PlanRecorder::new();
        register_ned_builtins(&root, store, &recorder);
        (root, recorder)
    }

    /// Evaluate `program` against `store` as a plan, then execute the plan
    /// (select-then-apply, as the conductor does).
    fn run(
        store: &Arc<RwLock<NodeStore>>,
        program: &str,
    ) -> Result<content_store::edit::Executed, String> {
        let root = RootEnv::new();
        crate::init_root(&root).expect("core init failed");
        let recorder = PlanRecorder::new();
        register_ned_builtins(&root, store.clone(), &recorder);
        crate::load_ned_prelude(&root).expect("ned prelude load failed");
        crate::eval_str_with_root(program, &root)?;
        let mut st = store.write().unwrap();
        content_store::edit::execute(&mut st, recorder.take()).map_err(|e| e.to_string())
    }

    /// A Clojure string literal for `s`.
    fn clj_str(s: &str) -> String {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
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

    // The "doc" the planning tests address: content/post/foo.md, body
    // paragraphs "First", "Second", "Third" (see `make_doc_store`).
    const FOO: &str = r#"(ned/doc-by-path "content/post/foo.md")"#;

    /// The texts of foo's body blocks, in document order (a selection is a
    /// set, so it cannot tell).
    fn foo_body_texts(store: &Arc<RwLock<NodeStore>>) -> Vec<String> {
        let root_env = setup_with_prelude(store.clone());
        let body = extract_selection(&eval(&root_env, &format!("(ned/slot {FOO} \"body\")")))
            .unwrap()
            .iter()
            .next()
            .expect("foo has a body");
        let st = store.read().unwrap();
        st.children(body)
            .into_iter()
            .map(|block| match st.children(block).first().and_then(|t| st.get(*t)) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("expected a text child, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn set_text_plans_and_executes() {
        let store = make_doc_store();
        let program = format!(r#"(ned/set-text (ned/text-in (ned/body-at {FOO} 1)) "Changed")"#);
        run(&store, &program).unwrap();
        assert_eq!(foo_body_texts(&store), vec!["First", "Changed", "Third"]);
    }

    /// The comments before foo's second body paragraph, as the store holds them.
    fn foo_second_comments(store: &Arc<RwLock<NodeStore>>) -> Vec<String> {
        let root_env = setup_with_prelude(store.clone());
        let block = extract_selection(&eval(&root_env, &format!("(ned/body-at {FOO} 1)")))
            .unwrap()
            .iter()
            .next()
            .expect("foo has a second block");
        let st = store.read().unwrap();
        ned::trivia::trivia_texts(&st, block, ned::trivia::ATTR_TRIVIA_BEFORE)
            .into_iter()
            .map(|t| match st.get(t) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// The comments at the end of foo's body, as the store holds them.
    fn foo_end_comments(store: &Arc<RwLock<NodeStore>>) -> Vec<String> {
        let root_env = setup_with_prelude(store.clone());
        let body = extract_selection(&eval(&root_env, &format!("(ned/slot {FOO} \"body\")")))
            .unwrap()
            .iter()
            .next()
            .expect("foo has a body");
        let st = store.read().unwrap();
        ned::trivia::trivia_texts(&st, body, ned::trivia::ATTR_TRIVIA_END)
            .into_iter()
            .map(|t| match st.get(t) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// LSP-2 2.1: `ned/set-comments-before` and `ned/set-end-comments` plan a
    /// SetComments, and a program runs it.
    #[test]
    fn set_comments_plans_and_executes() {
        let store = make_doc_store();
        let program = format!(
            r#"(ned/set-comments-before (ned/body-at {FOO} 1) ["<!-- a -->" "<!-- b -->"])"#
        );
        run(&store, &program).unwrap();
        assert_eq!(foo_second_comments(&store), vec!["<!-- a -->", "<!-- b -->"]);
        assert_eq!(foo_body_texts(&store), vec!["First", "Second", "Third"]);

        let program = format!(r#"(ned/set-end-comments (ned/slot {FOO} "body") ["<!-- end -->"])"#);
        run(&store, &program).unwrap();
        assert_eq!(foo_end_comments(&store), vec!["<!-- end -->"]);
        assert_eq!(foo_second_comments(&store), vec!["<!-- a -->", "<!-- b -->"]);

        // A string that is not a comment is refused, and nothing changes.
        let program = format!(r#"(ned/set-comments-before (ned/body-at {FOO} 1) ["text"])"#);
        let err = run(&store, &program).unwrap_err();
        assert!(
            err.starts_with("step 1 (ned/set-comments-before):") && err.contains("not an HTML comment"),
            "{err}"
        );
        assert_eq!(foo_second_comments(&store), vec!["<!-- a -->", "<!-- b -->"]);

        // The second argument must be a list of strings.
        let program = format!(r#"(ned/set-comments-before (ned/body-at {FOO} 1) "<!-- a -->")"#);
        assert!(run(&store, &program).is_err());

        // The place names the kind: end comments on a block are refused.
        let program = format!(r#"(ned/set-end-comments (ned/body-at {FOO} 1) ["<!-- x -->"])"#);
        let err = run(&store, &program).unwrap_err();
        assert!(err.starts_with("step 1 (ned/set-end-comments):"), "{err}");
    }

    #[test]
    fn set_text_over_several_texts_is_refused_and_applies_nothing() {
        let store = make_doc_store();
        let program = format!(
            r#"(ned/set-text (ned/texts (ned/descendants (ned/slot {FOO} "body"))) "X")"#
        );
        let err = run(&store, &program).unwrap_err();
        assert!(err.starts_with("step 1 (ned/set-text):") && err.contains("3 nodes"), "{err}");
        assert_eq!(foo_body_texts(&store), vec!["First", "Second", "Third"]);
    }

    #[test]
    fn search_replace_replaces_the_one_occurrence() {
        let store = make_doc_store();
        let program = format!(
            r#"(ned/search-replace (ned/text-in (ned/body-at {FOO} 1)) "eco" "ECO")"#
        );
        run(&store, &program).unwrap();
        assert_eq!(foo_body_texts(&store), vec!["First", "SECOnd", "Third"]);
    }

    #[test]
    fn search_replace_with_empty_search_is_refused() {
        let store = make_doc_store();
        let program = format!(r#"(ned/search-replace (ned/text-in (ned/body-at {FOO} 1)) "" "X")"#);
        let err = run(&store, &program).unwrap_err();
        assert!(err.contains("empty search text"), "{err}");
        assert_eq!(foo_body_texts(&store), vec!["First", "Second", "Third"]);
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
    fn insert_child_appends_the_tree_to_the_body() {
        let store = make_doc_store();
        let program = format!(
            r#"(ned/insert-child (ned/slot {FOO} "body")
                 (ned/with-child (ned/mk-element "paragraph") (ned/mk-text "Fourth")))"#
        );
        run(&store, &program).unwrap();
        assert_eq!(foo_body_texts(&store), vec!["First", "Second", "Third", "Fourth"]);
    }

    #[test]
    fn insert_before_and_after_place_the_tree_next_to_the_block() {
        let store = make_doc_store();
        let new = |text: &str| {
            format!(r#"(ned/with-child (ned/mk-element "paragraph") (ned/mk-text {}))"#, clj_str(text))
        };
        let program = format!(
            "(do (ned/insert-before (ned/body-at {FOO} 0) {}) (ned/insert-after (ned/body-at {FOO} 2) {}))",
            new("Zeroth"),
            new("Fourth")
        );
        run(&store, &program).unwrap();
        assert_eq!(
            foo_body_texts(&store),
            vec!["Zeroth", "First", "Second", "Third", "Fourth"]
        );
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
        register_ned_builtins(&root, store, &PlanRecorder::new());
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
                        node: ContentElement::heading_from_markdown(
                            HeadingLevel::new(1).unwrap(),
                            "My Title",
                        ),
                        span: Span { start: 0, end: 0 },
                    }],
                }
            ],
            body: im::vector![
                Spanned {
                    node: ContentElement::paragraph_from_markdown("First"),
                    span: Span { start: 0, end: 0 },
                },
                Spanned {
                    node: ContentElement::paragraph_from_markdown("Second"),
                    span: Span { start: 0, end: 0 },
                },
                Spanned {
                    node: ContentElement::paragraph_from_markdown("Third"),
                    span: Span { start: 0, end: 0 },
                },
            ],
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
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
            unassigned: Default::default(),
            comments: Default::default(),
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
    fn parse_body_returns_trees_and_does_not_touch_the_store() {
        let store = make_store_with_tree();
        let nodes_before = store.read().unwrap().node_count();
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
        assert_eq!(
            extract_node_tree(&trees[0]).expect("expected NodeTree"),
            NodeTree::element("paragraph").with_child(NodeTree::text("Hello world"))
        );
        assert_eq!(store.read().unwrap().node_count(), nodes_before, "the store is not touched");
    }

    #[test]
    fn parse_body_refuses_separator_and_html_block() {
        let store = make_store_with_tree();
        let root_env = setup(store);
        let grammar = make_grammar_value(&root_env);
        for (src, kind) in [("One.\n\n---\n\nTwo.\n", "separator"), ("<div>x</div>\n", "html-block")] {
            let err = call(
                &root_env,
                "ned/parse-body",
                vec![Value::Text(src.to_string()), grammar.clone()],
            )
            .expect_err(kind);
            assert!(
                err.contains(&format!("{} cannot be written through a program", schema::with_article(kind))),
                "{kind}: {err}"
            );
        }
    }

    #[test]
    fn parse_body_refuses_comments() {
        let store = make_store_with_tree();
        let root_env = setup(store);
        let grammar = make_grammar_value(&root_env);
        let err = call(
            &root_env,
            "ned/parse-body",
            vec![Value::Text("<!-- c -->\n\nOne.\n".to_string()), grammar],
        )
        .expect_err("comments are refused");
        assert!(
            err.contains("comments in a parse-body fragment are not supported yet"),
            "{err}"
        );
    }

    #[test]
    fn ned_store_is_not_bound() {
        let root_env = setup(make_store_with_tree());
        assert!(root_env.get("ned/store").is_none());
        assert!(!NED_MUTATING_BUILTINS.contains(&"ned/store"));
    }

    #[test]
    fn mutating_builtins_return_nil_and_record() {
        let store = make_store_with_tree();
        let (root, recorder) = setup_recording(store.clone());
        let nodes_before = store.read().unwrap().node_count();
        let all = call(&root, "ned/all", vec![]).unwrap();
        let texts_sel = call(
            &root,
            "ned/filter",
            vec![all, Value::Keyword { namespace: None, name: "text".to_string() }],
        )
        .unwrap();
        let tree = || call(&root, "ned/mk-element", vec![Value::Text("paragraph".to_string())]).unwrap();
        let text = |s: &str| Value::Text(s.to_string());
        let calls: Vec<(&str, Vec<Value>)> = vec![
            ("ned/set-text", vec![texts_sel.clone(), text("x")]),
            ("ned/search-replace", vec![texts_sel.clone(), text("o"), text("0")]),
            ("ned/delete", vec![texts_sel.clone()]),
            ("ned/insert-child", vec![texts_sel.clone(), tree()]),
            ("ned/insert-before", vec![texts_sel.clone(), tree()]),
            ("ned/insert-after", vec![texts_sel.clone(), tree()]),
            ("ned/replace", vec![texts_sel.clone(), Value::List(vec![tree()])]),
            ("ned/set-comments-before", vec![texts_sel.clone(), Value::List(vec![text("<!-- c -->")])]),
            ("ned/set-end-comments", vec![texts_sel.clone(), Value::List(vec![text("<!-- c -->")])]),
        ];
        for (name, args) in calls {
            let result = call(&root, name, args).unwrap();
            assert!(matches!(result, Value::Absent), "{name} returns nil, got {result:?}");
            assert_eq!(recorder.take().len(), 1, "{name} records exactly one step");
        }
        assert!(recorder.take().is_empty(), "take leaves the recorder empty");
        assert_eq!(store.read().unwrap().node_count(), nodes_before, "nothing is written");
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
                        node: ContentElement::heading_from_markdown(
                            HeadingLevel::new(1).unwrap(),
                            "Title",
                        ),
                        span: Span { start: 0, end: 0 },
                    }],
                }
            ],
            body: im::vector![
                Spanned {
                    node: ContentElement::paragraph_from_markdown("Old content"),
                    span: Span { start: 0, end: 0 },
                },
            ],
            has_separator: false,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let meta = DocumentMeta {
            url: "/post/replace-test".to_string(),
            stem: "post".to_string(),
            file: "content/post/replace-test.md".to_string(),
            page_kind: "item".to_string(),
        };
        document_to_store(&doc, &mut raw_store, Some(&meta));
        let store = Arc::new(RwLock::new(raw_store));

        let program = format!(
            "(let [g (ned/parse-grammar {})]
               (ned/replace
                 (ned/body-at (ned/doc-by-path \"content/post/replace-test.md\") 0)
                 (ned/parse-body {} g)))",
            clj_str(EMPTY_SCHEMA_SRC),
            clj_str("# New heading\n")
        );
        let executed = run(&store, &program).unwrap();
        assert_eq!(executed.documents.len(), 1);

        let root_env = setup_with_prelude(store.clone());
        let body = r#"(ned/body-at (ned/doc-by-path "content/post/replace-test.md") 0)"#;
        assert!(matches!(
            eval(&root_env, &format!("(ned/kind-of {body})")),
            Value::List(items) if matches!(items.as_slice(), [Value::Text(k)] if k == "heading")
        ));
        assert_eq!(texts_of(&root_env, &format!("(ned/text-in {body})")), vec!["New heading"]);
    }

    // ── HTML blocks (ADR-062 D4) ──────────────────────────────────────────────

    const HTML_SCHEMA_SRC: &str = "# Title {#title}\noccurs\n: exactly once\n\n----\n\nBody.\n";
    const HTML_DOC_SRC: &str = "# Title\n\n----\n\nBefore.\n\n<div class=\"x\">\nsecret\n</div>\n\nAfter.\n";

    /// A store holding `HTML_DOC_SRC` (body: paragraph, html-block, paragraph)
    /// plus the document root and the grammar.
    fn make_html_doc_store() -> (Arc<RwLock<NodeStore>>, node_store::NodeId, schema::Grammar) {
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

        let grammar = schema::parse_schema(HTML_SCHEMA_SRC).expect("schema parses");
        let doc = content::parse_and_assign(HTML_DOC_SRC, &grammar).expect("doc parses");
        assert!(!content::validate(&doc, &grammar).is_valid(), "the HTML block is an Error");
        let meta = DocumentMeta {
            url: "/post/html".to_string(),
            stem: "post".to_string(),
            file: "content/post/html.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut raw_store = NodeStore::new();
        let root = document_to_store(&doc, &mut raw_store, Some(&meta));
        (Arc::new(RwLock::new(raw_store)), root, grammar)
    }

    fn html_body_block(root_env: &RootEnv) -> Value {
        let block = crate::eval_str_with_root(
            r#"(ned/body-at (ned/doc-by-path "content/post/html.md") 1)"#,
            root_env,
        )
        .unwrap();
        let kind = crate::eval_str_with_root(
            r#"(ned/kind-of (ned/body-at (ned/doc-by-path "content/post/html.md") 1))"#,
            root_env,
        )
        .unwrap();
        assert!(
            matches!(&kind, Value::List(items) if matches!(items.as_slice(), [Value::Text(k)] if k == "html-block")),
            "body index 1 is the HTML block: {kind:?}"
        );
        block
    }

    fn saved_and_validity(
        store: &Arc<RwLock<NodeStore>>,
        root: node_store::NodeId,
        grammar: &schema::Grammar,
    ) -> (String, bool) {
        let st = store.read().unwrap();
        let saved = node_store_bridge::content_bridge::serialize_from_store(&st, root)
            .expect("document serializes");
        let doc = content::parse_and_assign(&saved, grammar).expect("saved file parses");
        let valid = content::validate(&doc, grammar).is_valid();
        (saved, valid)
    }

    #[test]
    fn delete_html_block_removes_it_and_validates_clean() {
        let (store, root, grammar) = make_html_doc_store();
        let root_env = setup_with_prelude(store.clone());
        html_body_block(&root_env);
        run(&store, r#"(ned/delete (ned/body-at (ned/doc-by-path "content/post/html.md") 1))"#)
            .unwrap();
        let (saved, valid) = saved_and_validity(&store, root, &grammar);
        assert!(!saved.contains("<div") && !saved.contains("secret"), "{saved}");
        assert!(saved.contains("Before.") && saved.contains("After."), "{saved}");
        assert!(valid, "no HTML left, so no Error: {saved}");
    }

    #[test]
    fn replace_html_block_with_a_paragraph_succeeds() {
        let (store, root, grammar) = make_html_doc_store();
        let root_env = setup_with_prelude(store.clone());
        html_body_block(&root_env);
        let program = format!(
            "(let [g (ned/parse-grammar {})]
               (ned/replace
                 (ned/body-at (ned/doc-by-path \"content/post/html.md\") 1)
                 (ned/parse-body {} g)))",
            clj_str(HTML_SCHEMA_SRC),
            clj_str("Replaced.\n")
        );
        run(&store, &program).unwrap();
        let (saved, valid) = saved_and_validity(&store, root, &grammar);
        assert!(!saved.contains("<div"), "{saved}");
        assert!(saved.contains("Before.\n\nReplaced.\n\nAfter."), "{saved}");
        assert!(valid, "{saved}");
    }

    // ── Comment trivia (ADR-062 D5) ───────────────────────────────────────────

    /// A store holding `src` (under `HTML_SCHEMA_SRC`) as content/post/html.md.
    fn make_doc_store_from(src: &str) -> (Arc<RwLock<NodeStore>>, node_store::NodeId, schema::Grammar) {
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
        let grammar = schema::parse_schema(HTML_SCHEMA_SRC).expect("schema parses");
        let doc = content::parse_and_assign(src, &grammar).expect("doc parses");
        let meta = DocumentMeta {
            url: "/post/html".to_string(),
            stem: "post".to_string(),
            file: "content/post/html.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut raw_store = NodeStore::new();
        let root = document_to_store(&doc, &mut raw_store, Some(&meta));
        (Arc::new(RwLock::new(raw_store)), root, grammar)
    }

    #[test]
    fn delete_html_block_moves_its_comment_to_the_next_block() {
        let (store, root, grammar) = make_doc_store_from(
            "# Title\n\n----\n\nBefore.\n\n<!-- keep -->\n\n<div>x</div>\n\nAfter.\n",
        );
        run(&store, r#"(ned/delete (ned/body-at (ned/doc-by-path "content/post/html.md") 1))"#)
            .unwrap();
        let (saved, valid) = saved_and_validity(&store, root, &grammar);
        assert_eq!(saved, "# Title\n\n----\n\nBefore.\n\n<!-- keep -->\n\nAfter.\n");
        assert!(valid, "{saved}");
    }

    #[test]
    fn deleting_the_last_block_moves_its_comment_to_the_body_end() {
        let (store, root, grammar) =
            make_doc_store_from("# Title\n\n----\n\nBefore.\n\n<!-- keep -->\n\n<div>x</div>\n");
        run(&store, r#"(ned/delete (ned/body-at (ned/doc-by-path "content/post/html.md") 1))"#)
            .unwrap();
        // The document still decodes and saves: the edit is accepted.
        let (saved, valid) = saved_and_validity(&store, root, &grammar);
        assert_eq!(saved, "# Title\n\n----\n\nBefore.\n\n<!-- keep -->\n");
        assert!(valid, "{saved}");
        let st = store.read().unwrap();
        let body = node_store_bridge::content_bridge::find_child_by_name(&st, root, "body").unwrap();
        assert_eq!(
            ned::trivia::trivia_texts(&st, body, ned::trivia::ATTR_TRIVIA_END).len(),
            1,
            "the comment is the body's end trivia"
        );
    }

    // ── Markers on text ──────────────────────────────────────────────────────

    fn set_body_paragraph_text(text: &str) -> (String, bool) {
        let (store, root, grammar) = make_doc_store_from("# Title\n\n----\n\nBefore.\n");
        let program = format!(
            r#"(ned/set-text (ned/filter (ned/descendants (ned/body-at (ned/doc-by-path "content/post/html.md") 0)) :text) {})"#,
            clj_str(text)
        );
        run(&store, &program).unwrap();
        saved_and_validity(&store, root, &grammar)
    }

    /// A paragraph's Text node is literal text (ADR-059 §6): one that ends in
    /// `{#summary}` is written escaped, so it is text and not marker misuse.
    #[test]
    fn set_text_ending_in_a_marker_shape_is_literal_and_saved_escaped() {
        let (saved, valid) = set_body_paragraph_text("Summary text {#summary}");
        assert!(saved.contains("Summary text \\{#summary}"), "{saved}");
        assert!(valid, "literal text is not marker misuse: {saved}");
    }

    /// A literal backslash before the marker shape is written as `\\`; the
    /// text still reads back as text.
    #[test]
    fn set_text_with_a_literal_backslash_before_a_marker_shape_stays_text() {
        let (saved, valid) = set_body_paragraph_text("Summary text \\{#summary}");
        assert!(saved.contains("Summary text \\\\{#summary}"), "{saved}");
        assert!(valid, "{saved}");
    }

    #[test]
    fn comments_do_not_shift_body_at_indices() {
        let (store, _root, _grammar) = make_doc_store_from(
            "# Title\n\n----\n\n<!-- a -->\n\nFirst.\n\n<!-- b -->\n\nSecond.\n",
        );
        let root_env = setup_with_prelude(store);
        let text = crate::eval_str_with_root(
            r#"(ned/text-of (ned/children (ned/body-at (ned/doc-by-path "content/post/html.md") 1)))"#,
            &root_env,
        )
        .unwrap();
        assert!(
            matches!(&text, Value::List(items) if matches!(items.as_slice(), [Value::Text(t)] if t == "Second.")),
            "{text:?}"
        );
    }

    #[test]
    fn structural_anchors_never_count_html_blocks() {
        // The browser addresses body blocks by kind (heading + nth paragraph
        // after it, or nth paragraph), so an HTML block never shifts them.
        let (store, _root, _grammar) =
            make_doc_store_from("# Title\n\n----\n\n### H\n\na\n\n<div>x</div>\n\nb\n");
        let root_env = setup_with_prelude(store);
        let body = r#"(ned/children (ned/slot (ned/doc-by-path "content/post/html.md") "body"))"#;
        let kinds = crate::eval_str_with_root(&format!("(ned/kind-of {body})"), &root_env).unwrap();
        assert!(
            matches!(&kinds, Value::List(items) if items.len() == 4
                && matches!(&items[2], Value::Text(k) if k == "html-block")),
            "{kinds:?}"
        );
        for selection in [
            format!(r#"(ned/nth-after (ned/heading-with-text {body} "H") "paragraph" 1)"#),
            format!(r#"(ned/nth-of-kind {body} "paragraph" 1)"#),
        ] {
            let text = crate::eval_str_with_root(
                &format!("(ned/text-of (ned/filter (ned/descendants {selection}) :text))"),
                &root_env,
            )
            .unwrap();
            assert!(
                matches!(&text, Value::List(items) if matches!(items.as_slice(), [Value::Text(t)] if t == "b")),
                "{selection}: {text:?}"
            );
        }
    }

    #[test]
    fn html_block_suggestion_payload_is_rejected() {
        use node_store_bridge::content_bridge::{tree_to_block, BlockDecodeError};
        let tree = NodeTree::element("html-block").with_child(NodeTree::text("<div>x</div>"));
        assert_eq!(
            tree_to_block(&tree),
            Err(BlockDecodeError::NotPayloadKind("html-block".to_string()))
        );
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
                    node: ContentElement::paragraph_from_markdown(text),
                    span: Span { start: 0, end: 0 },
                }],
                has_separator: false,
                separator_span: None,
                unassigned: Default::default(),
                comments: Default::default(),
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

    // ── Query-only vs mutation registration split ─────────────────────────────

    fn setup_query_only(store: Arc<RwLock<NodeStore>>) -> RootEnv {
        let root = RootEnv::new();
        crate::init_root(&root).expect("core init failed");
        register_ned_query_builtins(&root, store);
        root
    }

    #[test]
    fn mutating_builtins_are_registered_by_full_registration() {
        let store = make_store_with_tree();
        let root_env = setup(store);
        for name in NED_MUTATING_BUILTINS {
            assert!(
                matches!(root_env.get(name), Some(Value::Fn(_))),
                "{name} should be bound by register_ned_builtins"
            );
        }
    }

    #[test]
    fn mutating_builtins_fail_in_query_only_root_and_store_is_unchanged() {
        let store = make_store_with_tree();
        let count_before = store.read().unwrap().node_count();
        let root_env = setup_query_only(store.clone());
        for name in NED_MUTATING_BUILTINS {
            assert!(root_env.get(name).is_none(), "{name} must not be bound in a query-only root");
            let src = format!(r#"({name} (ned/all) "a" "b")"#);
            let result = crate::eval_str_with_root(&src, &root_env);
            assert!(result.is_err(), "{name} should fail to evaluate in a query-only root, got {result:?}");
            assert_eq!(
                store.read().unwrap().node_count(),
                count_before,
                "{name} must not change the store's node count"
            );
        }
    }

    #[test]
    fn ned_prelude_loads_and_runs_on_query_only_root() {
        let store = make_doc_store();
        let root_env = setup_query_only(store);
        crate::load_ned_prelude(&root_env).expect("ned prelude must load on a query-only root");

        let doc_count = crate::eval_str_with_root(
            r#"(ned/count (ned/doc-by-path "content/post/foo.md"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(doc_count, Value::Integer(1)));

        let slot_count = crate::eval_str_with_root(
            r#"(ned/count (ned/slot (ned/doc-by-path "content/post/foo.md") "title"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(slot_count, Value::Integer(1)));

        let heading_count = crate::eval_str_with_root(
            r#"(ned/count (ned/heading-with-text (ned/descendants (ned/doc-by-path "content/post/foo.md")) "My Title"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(heading_count, Value::Integer(1)), "got {heading_count:?}");
    }

    // ── Body-slot structural selectors (as emitted by inject.js buildBodyAnchor) ──

    /// Document at `content/index.md` whose body is:
    /// H2 "Intro", P "Intro para one", P "Intro para two", H2 "Why Presemble", P "Why para".
    fn make_structured_body_store() -> Arc<RwLock<NodeStore>> {
        use content::{ContentElement, Document, DocumentSlot};
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
        use schema::{HeadingLevel, SlotName, Span, Spanned};

        fn spanned(node: ContentElement) -> Spanned<ContentElement> {
            Spanned { node, span: Span { start: 0, end: 0 } }
        }
        fn h2(text: &str) -> Spanned<ContentElement> {
            spanned(ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), text))
        }
        fn p(text: &str) -> Spanned<ContentElement> {
            spanned(ContentElement::paragraph_from_markdown(text))
        }

        let doc = Document {
            preamble: im::vector![DocumentSlot {
                name: SlotName::new("title"),
                elements: im::vector![spanned(ContentElement::heading_from_markdown(
                    HeadingLevel::new(1).unwrap(),
                    "Home",
                ))],
            }],
            body: im::vector![
                h2("Intro"),
                p("Intro para one"),
                p("Intro para two"),
                h2("Why Presemble"),
                p("Why para"),
            ],
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let meta = DocumentMeta {
            url: "/".to_string(),
            stem: "index".to_string(),
            file: "content/index.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut store = NodeStore::new();
        document_to_store(&doc, &mut store, Some(&meta));
        Arc::new(RwLock::new(store))
    }

    /// Selector strings exactly as `inject.js` builds them (cf. the
    /// `derive_anchor_structural_*` tests in publisher_cli serve.rs), paired with
    /// the expected (kind, text) of the single node they must resolve to.
    const BODY_SELECTOR_CASES: &[(&str, &str, &str)] = &[
        // nth-after / heading-with-text
        (
            r#"(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Intro") "paragraph" 0)"#,
            "paragraph",
            "Intro para one",
        ),
        (
            r#"(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Intro") "paragraph" 1)"#,
            "paragraph",
            "Intro para two",
        ),
        (
            r#"(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Why Presemble") "paragraph" 0)"#,
            "paragraph",
            "Why para",
        ),
        // heading-with-text
        (
            r#"(ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Intro")"#,
            "heading",
            "Intro",
        ),
        (
            r#"(ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "Why Presemble")"#,
            "heading",
            "Why Presemble",
        ),
        // nth-of-kind
        (
            r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "paragraph" 2)"#,
            "paragraph",
            "Why para",
        ),
        (
            r#"(ned/nth-of-kind (ned/children (ned/slot (ned/doc-by-path "content/index.md") "body")) "heading" 1)"#,
            "heading",
            "Why Presemble",
        ),
        // nth-child fallback
        (
            r#"(ned/nth-child (ned/slot (ned/doc-by-path "content/index.md") "body") 2)"#,
            "paragraph",
            "Intro para two",
        ),
        (
            r#"(ned/nth-child (ned/slot (ned/doc-by-path "content/index.md") "body") 3)"#,
            "heading",
            "Why Presemble",
        ),
    ];

    fn assert_resolves_to(root_env: &RootEnv, sel: &str, kind: &str, text: &str) {
        let count = crate::eval_str_with_root(&format!("(ned/count {sel})"), root_env)
            .unwrap_or_else(|e| panic!("count eval failed for {sel}: {e}"));
        assert!(
            matches!(count, Value::Integer(1)),
            "expected exactly one node for {sel}, got {count:?}"
        );

        let kinds = crate::eval_str_with_root(&format!("(ned/kind-of {sel})"), root_env)
            .unwrap_or_else(|e| panic!("kind-of eval failed for {sel}: {e}"));
        match kinds {
            Value::List(items) => {
                assert_eq!(items.len(), 1, "kind-of for {sel}: {items:?}");
                assert!(
                    matches!(&items[0], Value::Text(s) if s == kind),
                    "expected kind {kind:?} for {sel}, got {items:?}"
                );
            }
            other => panic!("expected List from ned/kind-of, got {other:?}"),
        }

        let texts = crate::eval_str_with_root(
            &format!("(ned/text-of (ned/filter (ned/descendants {sel}) :text))"),
            root_env,
        )
        .unwrap_or_else(|e| panic!("text-of eval failed for {sel}: {e}"));
        match texts {
            Value::List(items) => {
                assert_eq!(items.len(), 1, "text-of for {sel}: {items:?}");
                assert!(
                    matches!(&items[0], Value::Text(s) if s == text),
                    "expected text {text:?} for {sel}, got {items:?}"
                );
            }
            other => panic!("expected List from ned/text-of, got {other:?}"),
        }
    }

    #[test]
    fn slot_body_returns_the_body_element() {
        let store = make_structured_body_store();
        let root_env = setup_with_prelude(store);
        let kinds = crate::eval_str_with_root(
            r#"(ned/kind-of (ned/slot (ned/doc-by-path "content/index.md") "body"))"#,
            &root_env,
        )
        .unwrap();
        match kinds {
            Value::List(items) => {
                assert_eq!(items.len(), 1, "got {items:?}");
                assert!(matches!(&items[0], Value::Text(s) if s == "body"), "got {items:?}");
            }
            other => panic!("expected List, got {other:?}"),
        }
        let body_count = crate::eval_str_with_root(
            r#"(ned/count (ned/body (ned/doc-by-path "content/index.md")))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(body_count, Value::Integer(1)), "got {body_count:?}");
        // Preamble slots still resolve as before.
        let title_count = crate::eval_str_with_root(
            r#"(ned/count (ned/slot (ned/doc-by-path "content/index.md") "title"))"#,
            &root_env,
        )
        .unwrap();
        assert!(matches!(title_count, Value::Integer(1)), "got {title_count:?}");
    }

    #[test]
    fn inject_js_body_selectors_resolve_to_single_expected_node() {
        let store = make_structured_body_store();
        let root_env = setup_with_prelude(store);
        for (sel, kind, text) in BODY_SELECTOR_CASES {
            assert_resolves_to(&root_env, sel, kind, text);
        }
    }

    #[test]
    fn inject_js_body_selectors_resolve_in_query_only_root() {
        let store = make_structured_body_store();
        let root_env = setup_query_only(store);
        crate::load_ned_prelude(&root_env).expect("ned prelude must load on a query-only root");
        for (sel, kind, text) in BODY_SELECTOR_CASES {
            assert_resolves_to(&root_env, sel, kind, text);
        }
    }

    // ── Text-addressing helpers (ADR-056) ─────────────────────────────────────

    const BODY: &str = r#"(ned/slot (ned/doc-by-path "content/index.md") "body")"#;

    fn eval(root_env: &RootEnv, src: &str) -> Value {
        crate::eval_str_with_root(src, root_env).unwrap_or_else(|e| panic!("eval failed for {src}: {e}"))
    }

    fn count_of(root_env: &RootEnv, sel: &str) -> i64 {
        match eval(root_env, &format!("(ned/count {sel})")) {
            Value::Integer(n) => n,
            other => panic!("expected Integer from ned/count, got {other:?}"),
        }
    }

    fn texts_of(root_env: &RootEnv, sel: &str) -> Vec<String> {
        match eval(root_env, &format!("(ned/text-of {sel})")) {
            Value::List(items) => items
                .into_iter()
                .map(|v| match v {
                    Value::Text(s) => s,
                    other => panic!("expected Text, got {other:?}"),
                })
                .collect(),
            other => panic!("expected List from ned/text-of, got {other:?}"),
        }
    }

    /// Body: H2 "Example", P "Alpha", H2 "Example", P "Beta".
    fn make_duplicate_heading_store() -> Arc<RwLock<NodeStore>> {
        use content::{ContentElement, Document};
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};
        use schema::{HeadingLevel, Span, Spanned};

        fn spanned(node: ContentElement) -> Spanned<ContentElement> {
            Spanned { node, span: Span { start: 0, end: 0 } }
        }
        let h2 = |t: &str| spanned(ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), t));
        let p = |t: &str| spanned(ContentElement::paragraph_from_markdown(t));

        let doc = Document {
            preamble: im::vector![],
            body: im::vector![h2("Example"), p("Alpha"), h2("Example"), p("Beta")],
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let meta = DocumentMeta {
            url: "/".to_string(),
            stem: "index".to_string(),
            file: "content/index.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut store = NodeStore::new();
        document_to_store(&doc, &mut store, Some(&meta));
        Arc::new(RwLock::new(store))
    }

    /// Body parsed from `markdown`, stored for `content/index.md`.
    fn make_markdown_body_store(markdown: &str) -> Arc<RwLock<NodeStore>> {
        use content::Document;
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store};

        let flat = content::parse_document(markdown).expect("markdown parses");
        let doc = Document {
            preamble: im::vector![],
            body: flat.elements,
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let meta = DocumentMeta {
            url: "/".to_string(),
            stem: "index".to_string(),
            file: "content/index.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut store = NodeStore::new();
        document_to_store(&doc, &mut store, Some(&meta));
        Arc::new(RwLock::new(store))
    }

    /// `ned/text-in` of a formatted heading is every Text node, at any depth,
    /// not just the direct ones: a SetText on it is ambiguous, never applied to
    /// a part of the heading (ADR-059 §9).
    #[test]
    fn text_in_a_formatted_heading_is_ambiguous_not_partial() {
        let root_env = setup_with_prelude(make_markdown_body_store("## Hello *World*\n\nPara.\n"));
        let heading = format!(r#"(ned/nth-of-kind (ned/blocks {BODY}) "heading" 0)"#);
        assert_eq!(
            texts_of(&root_env, &format!("(ned/text-in {heading})")),
            vec!["Hello ".to_string(), "World".to_string()],
            "both Text nodes, including the one inside the emphasis"
        );
        // Several nodes: `resolve` refuses a SetText on them as ambiguous.
        assert_eq!(count_of(&root_env, &format!("(ned/text-in {heading})")), 2);
    }

    /// Addresses are unchanged by headings holding nodes: `heading-with-text`
    /// matches the plain text of every heading in the shared vectors, and
    /// `block_addresses` writes the same text (guard).
    #[test]
    fn heading_addresses_unchanged() {
        // (heading source, plain text), a selection of the shared vectors.
        let cases = [
            ("Hello *World*", "Hello World"),
            ("The `<div>` element", "The <div> element"),
            ("A [link](/x) here", "A link here"),
            ("Title \\#", "Title #"),
            ("![i](p.png) t", "t"),
            ("**b** and _i_", "b and i"),
            ("Plain", "Plain"),
        ];
        let mut checked = 0;
        for (source, plain) in cases {
            let markdown = format!("## {source}\n\nPara.\n");
            let flat = content::parse_document(&markdown).expect("parses");
            assert_eq!(flat.elements.len(), 2, "{source:?} is one heading and a paragraph");
            let root_env = setup_with_prelude(make_markdown_body_store(&markdown));
            let with_text = format!(
                r#"(ned/heading-with-text (ned/blocks {BODY}) {})"#,
                content::clj_str(plain)
            );
            assert_eq!(count_of(&root_env, &with_text), 1, "{source:?} -> {plain:?}");
            let addresses = content::block_addresses(
                "content/index.md",
                content::AddressContainer::Body,
                &flat.elements.iter().map(|e| e.node.clone()).collect::<Vec<_>>(),
            );
            assert!(
                addresses[0].as_deref().is_some_and(|a| a.contains(&content::clj_str(plain))),
                "{source:?}: {addresses:?}"
            );
            checked += 1;
        }
        assert!(checked > 5, "the vectors exercised: {checked}");
    }

    /// Headings hold inline nodes; `ned/heading-with-text` matches their plain
    /// text, as rendered (ADR-059 §8).
    #[test]
    fn heading_with_text_matches_plain_text() {
        let root_env =
            setup_with_prelude(make_markdown_body_store("## The `<div>` element\n\nPara.\n"));
        let heading = format!(r#"(ned/nth-of-kind (ned/blocks {BODY}) "heading" 0)"#);
        assert_eq!(
            texts_of(&root_env, &format!("(ned/text-in {heading})")),
            vec!["The ".to_string(), "<div>".to_string(), " element".to_string()],
            "the store holds nodes: text, a code span, text"
        );
        let with_text = |t: &str| {
            format!(r#"(ned/heading-with-text (ned/blocks {BODY}) "{}")"#, t.replace('"', "\\\""))
        };
        assert_eq!(count_of(&root_env, &with_text("The <div> element")), 1);
        assert_eq!(count_of(&root_env, &with_text("The `<div>` element")), 0);

        // Two headings with equal plain text both match (ambiguous to address).
        let root_env = setup_with_prelude(make_markdown_body_store("## *A*\n\n## A\n"));
        assert_eq!(count_of(&root_env, &with_text("A")), 2);
    }

    #[test]
    fn nth_after_with_duplicated_heading_returns_two_nodes() {
        let store = make_duplicate_heading_store();
        let root_env = setup_with_prelude(store);
        let sel = format!(r#"(ned/nth-after (ned/heading-with-text (ned/blocks {BODY}) "Example") "paragraph" 0)"#);
        assert_eq!(count_of(&root_env, &sel), 2, "each anchor heading resolves separately");
        let mut texts = texts_of(&root_env, &format!("(ned/texts-under {sel})"));
        texts.sort();
        assert_eq!(texts, vec!["Alpha".to_string(), "Beta".to_string()]);

        // first-after behaves the same way.
        let sel = format!(r#"(ned/first-after (ned/heading-with-text (ned/blocks {BODY}) "Example") "paragraph")"#);
        assert_eq!(count_of(&root_env, &sel), 2);

        // idx 1 after the first heading is "Beta"; after the second there is none.
        let sel = format!(r#"(ned/nth-after (ned/heading-with-text (ned/blocks {BODY}) "Example") "paragraph" 1)"#);
        assert_eq!(texts_of(&root_env, &format!("(ned/texts-under {sel})")), vec!["Beta".to_string()]);
    }

    #[test]
    fn ned_blocks_returns_body_children() {
        let root_env = setup_with_prelude(make_structured_body_store());
        assert_eq!(count_of(&root_env, &format!("(ned/blocks {BODY})")), 5);
    }

    #[test]
    fn ned_text_in_returns_every_text_of_a_block() {
        let root_env = setup_with_prelude(make_structured_body_store());
        let sel = format!(r#"(ned/text-in (ned/nth-of-kind (ned/blocks {BODY}) "paragraph" 1))"#);
        assert_eq!(count_of(&root_env, &sel), 1);
        assert_eq!(texts_of(&root_env, &sel), vec!["Intro para two".to_string()]);
        // At any depth: the whole body holds five Text nodes.
        assert_eq!(count_of(&root_env, &format!("(ned/text-in {BODY})")), 5);
    }

    #[test]
    fn ned_texts_under_returns_all_descendant_texts() {
        let root_env = setup_with_prelude(make_structured_body_store());
        assert_eq!(count_of(&root_env, &format!("(ned/texts-under {BODY})")), 5);
    }

    #[test]
    fn ned_nth_text_per_scope_node() {
        let root_env = setup_with_prelude(make_structured_body_store());
        assert_eq!(texts_of(&root_env, &format!("(ned/nth-text {BODY} 0)")), vec!["Intro".to_string()]);
        assert_eq!(texts_of(&root_env, &format!("(ned/nth-text {BODY} 4)")), vec!["Why para".to_string()]);
        assert_eq!(count_of(&root_env, &format!("(ned/nth-text {BODY} 5)")), 0, "out of range is empty");
        // Scope of all paragraphs: one result per paragraph.
        assert_eq!(count_of(&root_env, &format!(r#"(ned/nth-text (ned/filter (ned/blocks {BODY}) :kind "paragraph") 0)"#)), 3);
        // Negative idx is an error.
        assert!(crate::eval_str_with_root(&format!("(ned/nth-text {BODY} -1)"), &root_env).is_err());
    }

    #[test]
    fn ned_nth_following_of_kind_negative_idx_is_error() {
        let root_env = setup_with_prelude(make_structured_body_store());
        let src = format!(r#"(ned/nth-following-of-kind (ned/nth-of-kind (ned/blocks {BODY}) "heading" 0) "paragraph" -1)"#);
        assert!(crate::eval_str_with_root(&src, &root_env).is_err());
        let src = format!(r#"(ned/nth-following-of-kind (ned/nth-of-kind (ned/blocks {BODY}) "heading" 0) "paragraph" 9)"#);
        assert_eq!(count_of(&root_env, &src), 0, "out of range is empty");
    }

    #[test]
    fn ned_first_text_returns_first_text_per_scope_node() {
        let root_env = setup_with_prelude(make_structured_body_store());
        let sel = format!(r#"(ned/first-text (ned/heading-with-text (ned/blocks {BODY}) "Why Presemble"))"#);
        assert_eq!(texts_of(&root_env, &sel), vec!["Why Presemble".to_string()]);
        assert_eq!(texts_of(&root_env, &format!("(ned/first-text {BODY})")), vec!["Intro".to_string()]);
    }

    #[test]
    fn ned_text_containing_returns_matching_text_nodes() {
        let root_env = setup_with_prelude(make_structured_body_store());
        let sel = format!(r#"(ned/text-containing {BODY} "Why")"#);
        assert_eq!(count_of(&root_env, &sel), 2, "ambiguous: heading text and paragraph text");
        let sel = format!(r#"(ned/text-containing {BODY} "Why para")"#);
        assert_eq!(texts_of(&root_env, &sel), vec!["Why para".to_string()]);
        // Results are Text nodes, not their parent blocks.
        assert!(
            matches!(eval(&root_env, &format!("(ned/kind-of {sel})")), Value::List(ref items) if items.is_empty()),
            "text-containing must yield Text nodes, not elements"
        );
        assert_eq!(count_of(&root_env, &format!(r#"(ned/text-containing {BODY} "nope")"#)), 0);
    }

    #[test]
    fn ned_filter_text_predicates_match_text_nodes() {
        let root_env = setup_with_prelude(make_structured_body_store());
        let sel = format!(r#"(ned/filter (ned/texts-under {BODY}) :text-equals "Intro")"#);
        assert_eq!(texts_of(&root_env, &sel), vec!["Intro".to_string()]);
        let sel = format!(r#"(ned/filter (ned/texts-under {BODY}) :text-contains "para")"#);
        assert_eq!(count_of(&root_env, &sel), 3);
    }

    #[test]
    fn new_text_helpers_run_on_query_only_root() {
        let root_env = setup_query_only(make_structured_body_store());
        crate::load_ned_prelude(&root_env).expect("ned prelude must load on a query-only root");
        for (sel, expected) in [
            (format!("(ned/blocks {BODY})"), 5),
            (format!(r#"(ned/text-in (ned/nth-of-kind (ned/blocks {BODY}) "paragraph" 0))"#), 1),
            (format!("(ned/texts-under {BODY})"), 5),
            (format!("(ned/nth-text {BODY} 1)"), 1),
            (format!("(ned/first-text {BODY})"), 1),
            (format!(r#"(ned/text-containing {BODY} "Intro para")"#), 2),
            (format!(r#"(ned/nth-following-of-kind (ned/nth-of-kind (ned/blocks {BODY}) "heading" 0) "paragraph" 1)"#), 1),
            (format!(r#"(ned/nth-after (ned/nth-of-kind (ned/blocks {BODY}) "heading" 0) "paragraph" 1)"#), 1),
            (format!(r#"(ned/first-after (ned/nth-of-kind (ned/blocks {BODY}) "heading" 1) "paragraph")"#), 1),
        ] {
            assert_eq!(count_of(&root_env, &sel), expected, "for {sel}");
        }
    }

    #[test]
    fn ned_prelude_references_no_mutating_builtin() {
        let tokens: Vec<&str> = crate::NED_PRELUDE
            .split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '[' || c == ']')
            .collect();
        for name in NED_MUTATING_BUILTINS {
            assert!(!tokens.contains(name), "ned.clj must not reference mutating builtin {name}");
        }
    }

    #[test]
    fn new_query_builtins_are_not_mutating() {
        for name in ["ned/nth-text", "ned/nth-following-of-kind"] {
            assert!(!NED_MUTATING_BUILTINS.contains(&name));
        }
    }

    #[test]
    fn body_at_still_picks_idx_via_ned_body() {
        let store = make_structured_body_store();
        let root_env = setup_with_prelude(store);
        assert_resolves_to(
            &root_env,
            r#"(ned/body-at (ned/doc-by-path "content/index.md") 4)"#,
            "paragraph",
            "Why para",
        );
    }
}
