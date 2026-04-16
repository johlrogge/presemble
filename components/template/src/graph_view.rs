use node_store::{NodeId, NodeStore};

use crate::data::{DataGraph, Value};

/// A reference to data that may be borrowed or owned.
pub enum DataRef<'a> {
    Borrowed(&'a Value),
    Owned(Value),
}

impl<'a> DataRef<'a> {
    pub fn as_value(&self) -> &Value {
        match self {
            DataRef::Borrowed(v) => v,
            DataRef::Owned(v) => v,
        }
    }

    pub fn into_owned(self) -> Value {
        match self {
            DataRef::Borrowed(v) => v.clone(),
            DataRef::Owned(v) => v,
        }
    }
}

/// A resolved node reference — provides direct access to the NodeStore
/// without materializing to Value. Used by the render hot path.
pub struct ResolvedNode<'a> {
    pub id: NodeId,
    pub store: &'a NodeStore,
}

/// Uniform data access for template rendering.
/// Abstracts over DataGraph (legacy) and NodeStore (new).
pub trait GraphView {
    /// Resolve a dot-separated path to a value.
    fn resolve(&self, path: &[&str]) -> Option<DataRef<'_>>;

    /// List all top-level keys.
    fn iter_keys(&self) -> Vec<String>;

    /// Create a new GraphView scoped to the node at the given path.
    /// Returns None if the path does not resolve to a Record/Element.
    fn clone_scoped(&self, path: &[&str]) -> Option<Box<dyn GraphView + '_>>;

    /// Clone the entire view and insert a binding. Used by data-each iteration.
    fn with_binding(&self, key: String, value: Value) -> Box<dyn GraphView>;

    /// Resolve a path to a NodeId + store reference, avoiding Value materialization.
    /// Returns None by default — only NodeStore-backed implementations provide this.
    fn resolve_node(&self, _path: &[&str]) -> Option<ResolvedNode<'_>> {
        None
    }
}

impl GraphView for DataGraph {
    fn resolve(&self, path: &[&str]) -> Option<DataRef<'_>> {
        // Calls DataGraph::resolve (inherent), not GraphView::resolve
        DataGraph::resolve(self, path).map(DataRef::Borrowed)
    }

    fn iter_keys(&self) -> Vec<String> {
        self.iter().map(|(k, _)| k.clone()).collect()
    }

    fn clone_scoped(&self, path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
        match DataGraph::resolve(self, path) {
            Some(Value::Record(sub)) => Some(Box::new(sub.clone())),
            _ => None,
        }
    }

    fn with_binding(&self, key: String, value: Value) -> Box<dyn GraphView> {
        let mut cloned = self.clone();
        cloned.insert(key, value);
        Box::new(cloned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{DataGraph, Value};

    #[test]
    fn datagraph_resolve_text() {
        let mut g = DataGraph::new();
        g.insert("title", Value::Text("Hello".into()));
        let view: &dyn GraphView = &g;
        let result = view.resolve(&["title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Hello"));
    }

    #[test]
    fn datagraph_resolve_nested() {
        let mut inner = DataGraph::new();
        inner.insert("name", Value::Text("Alice".into()));
        let mut g = DataGraph::new();
        g.insert("author", Value::Record(inner));
        let view: &dyn GraphView = &g;
        let result = view.resolve(&["author", "name"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Alice"));
    }

    #[test]
    fn datagraph_resolve_absent() {
        let g = DataGraph::new();
        let view: &dyn GraphView = &g;
        assert!(view.resolve(&["missing"]).is_none());
    }

    #[test]
    fn datagraph_iter_keys() {
        let mut g = DataGraph::new();
        g.insert("a", Value::Text("1".into()));
        g.insert("b", Value::Text("2".into()));
        let view: &dyn GraphView = &g;
        let mut keys = view.iter_keys();
        keys.sort();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[test]
    fn datagraph_clone_scoped() {
        let mut inner = DataGraph::new();
        inner.insert("x", Value::Text("val".into()));
        let mut g = DataGraph::new();
        g.insert("sub", Value::Record(inner));
        let view: &dyn GraphView = &g;
        let scoped = view.clone_scoped(&["sub"]).unwrap();
        let result = scoped.resolve(&["x"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "val"));
    }

    #[test]
    fn datagraph_with_binding() {
        let mut g = DataGraph::new();
        g.insert("a", Value::Text("original".into()));
        let view: &dyn GraphView = &g;
        let bound = view.with_binding("b".into(), Value::Text("added".into()));
        assert!(bound.resolve(&["a"]).is_some());
        assert!(bound.resolve(&["b"]).is_some());
    }
}
