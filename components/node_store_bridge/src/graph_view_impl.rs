//! GraphView implementations backed by NodeStore.
//!
//! `NodeStoreView` wraps a `&NodeStore` + root `NodeId` and implements `GraphView`
//! by walking ConsistsOf, Reference, and Attribute edges.
//!
//! `LayeredGraphView` overlays local bindings on top of any parent `GraphView`.

use std::collections::HashMap;

use node_store::{NodeId, NodeStore};
use template::data::Value;
use template::graph_view::{DataRef, GraphView, ResolvedNode};
use template::DataGraph;

use crate::value_bridge::node_to_value;

// ---------------------------------------------------------------------------
// NodeStoreView
// ---------------------------------------------------------------------------

/// A `GraphView` backed by a `NodeStore`, rooted at a specific `NodeId`.
///
/// `resolve` walks ConsistsOf edges first, then Reference edges, then Attribute
/// edges for each path segment. At the terminal node the subtree is materialised
/// via `node_to_value`.
pub struct NodeStoreView<'a> {
    store: &'a NodeStore,
    root: NodeId,
}

impl<'a> NodeStoreView<'a> {
    pub fn new(store: &'a NodeStore, root: NodeId) -> Self {
        Self { store, root }
    }

    pub fn store(&self) -> &'a NodeStore {
        self.store
    }

    pub fn root(&self) -> NodeId {
        self.root
    }
}

impl<'a> GraphView for NodeStoreView<'a> {
    fn resolve(&self, path: &[&str]) -> Option<DataRef<'_>> {
        let mut current = self.root;
        for (i, segment) in path.iter().enumerate() {
            let is_last = i == path.len() - 1;

            // 1. Try ConsistsOf edges (structural composition)
            let parts = self.store.consists_of(current);
            let part_target = parts
                .iter()
                .find(|(name, _)| self.store.resolve_name(*name) == *segment)
                .map(|(_, id)| *id);

            if let Some(id) = part_target {
                if is_last {
                    return Some(DataRef::Owned(node_to_value(self.store, id)));
                }
                current = id;
                continue;
            }

            // 2. Try Reference edges (cross-document links)
            let refs = self.store.references(current);
            let ref_target = refs
                .iter()
                .find(|(name, _)| self.store.resolve_name(*name) == *segment)
                .map(|(_, id)| *id);

            if let Some(id) = ref_target {
                if is_last {
                    return Some(DataRef::Owned(node_to_value(self.store, id)));
                }
                current = id;
                continue;
            }

            // 3. Fall back to Attribute edges
            let attrs = self.store.attributes(current);
            let attr_target = attrs
                .iter()
                .find(|(name, _)| self.store.resolve_name(*name) == *segment)
                .map(|(_, id)| *id);

            match attr_target {
                Some(id) if is_last => {
                    return Some(DataRef::Owned(node_to_value(self.store, id)));
                }
                Some(id) => {
                    current = id;
                }
                None => return None,
            }
        }
        // Empty path — nothing to resolve
        None
    }

    fn iter_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .store
            .consists_of(self.root)
            .iter()
            .map(|(name, _)| self.store.resolve_name(*name).to_string())
            .collect();
        for (name, _) in self.store.references(self.root) {
            keys.push(self.store.resolve_name(name).to_string());
        }
        for (name, _) in self.store.attributes(self.root) {
            keys.push(self.store.resolve_name(name).to_string());
        }
        keys
    }

    fn clone_scoped(&self, path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
        let mut current = self.root;
        for segment in path {
            // Try ConsistsOf first
            let parts = self.store.consists_of(current);
            let part_target = parts
                .iter()
                .find(|(name, _)| self.store.resolve_name(*name) == *segment)
                .map(|(_, id)| *id);
            if let Some(id) = part_target {
                current = id;
                continue;
            }
            // Then Reference
            let refs = self.store.references(current);
            let target = refs
                .iter()
                .find(|(name, _)| self.store.resolve_name(*name) == *segment)
                .map(|(_, id)| *id);
            match target {
                Some(id) => current = id,
                None => return None,
            }
        }
        Some(Box::new(NodeStoreView::new(self.store, current)))
    }

    fn resolve_node(&self, path: &[&str]) -> Option<ResolvedNode<'_>> {
        let mut current = self.root;
        for (i, segment) in path.iter().enumerate() {
            let is_last = i == path.len() - 1;

            // Try ConsistsOf edges first
            let parts = self.store.consists_of(current);
            if let Some((_, id)) = parts.iter().find(|(name, _)| self.store.resolve_name(*name) == *segment) {
                if is_last {
                    return Some(ResolvedNode { id: *id, store: self.store });
                }
                current = *id;
                continue;
            }

            // Try Reference edges
            let refs = self.store.references(current);
            if let Some((_, id)) = refs.iter().find(|(name, _)| self.store.resolve_name(*name) == *segment) {
                if is_last {
                    return Some(ResolvedNode { id: *id, store: self.store });
                }
                current = *id;
                continue;
            }

            // Try Attribute edges
            let attrs = self.store.attributes(current);
            if let Some((_, id)) = attrs.iter().find(|(name, _)| self.store.resolve_name(*name) == *segment) {
                if is_last {
                    return Some(ResolvedNode { id: *id, store: self.store });
                }
                current = *id;
                continue;
            }

            return None;
        }
        None
    }

    fn with_binding(&self, key: String, value: Value) -> Box<dyn GraphView> {
        // Materialise the current node as a DataGraph, then add the binding.
        let parent_value = node_to_value(self.store, self.root);
        let mut combined = match parent_value {
            Value::Record(g) => g,
            other => {
                let mut g = DataGraph::new();
                g.insert("value", other);
                g
            }
        };
        combined.insert(key, value);
        Box::new(combined)
    }
}

// ---------------------------------------------------------------------------
// LayeredGraphView
// ---------------------------------------------------------------------------

/// A `GraphView` that overlays local bindings on top of a parent `GraphView`.
///
/// Local bindings take priority; all other lookups are delegated to the parent.
pub struct LayeredGraphView {
    bindings: HashMap<String, Value>,
    parent: Box<dyn GraphView>,
}

impl LayeredGraphView {
    pub fn new(bindings: HashMap<String, Value>, parent: Box<dyn GraphView>) -> Self {
        Self { bindings, parent }
    }
}

impl GraphView for LayeredGraphView {
    fn resolve(&self, path: &[&str]) -> Option<DataRef<'_>> {
        match path {
            [] => None,
            [key, rest @ ..] => {
                if let Some(value) = self.bindings.get(*key) {
                    if rest.is_empty() {
                        Some(DataRef::Borrowed(value))
                    } else {
                        // Navigate into the bound value — must return Owned because
                        // the sub-value has no lifetime tied to &self directly.
                        match value {
                            Value::Record(sub) => sub
                                .resolve(rest)
                                .map(|v| DataRef::Owned(v.clone())),
                            _ => None,
                        }
                    }
                } else {
                    self.parent.resolve(path)
                }
            }
        }
    }

    fn iter_keys(&self) -> Vec<String> {
        let mut keys = self.parent.iter_keys();
        for k in self.bindings.keys() {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
        keys
    }

    fn clone_scoped(&self, path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
        match path {
            [] => None,
            [key, rest @ ..] => {
                if let Some(value) = self.bindings.get(*key) {
                    match value {
                        Value::Record(sub) => {
                            if rest.is_empty() {
                                Some(Box::new(sub.clone()))
                            } else {
                                sub.clone_scoped(rest)
                            }
                        }
                        _ => None,
                    }
                } else {
                    self.parent.clone_scoped(path)
                }
            }
        }
    }

    fn with_binding(&self, key: String, value: Value) -> Box<dyn GraphView> {
        // Materialise parent keys into a DataGraph, merge existing bindings, add new one.
        let mut graph = DataGraph::new();
        for k in self.parent.iter_keys() {
            if let Some(v) = self.parent.resolve(&[k.as_str()]) {
                graph.insert(k, v.into_owned());
            }
        }
        for (k, v) in &self.bindings {
            graph.insert(k.clone(), v.clone());
        }
        graph.insert(key, value);
        Box::new(graph)
    }
}

// ---------------------------------------------------------------------------
// PrefixedGraphView
// ---------------------------------------------------------------------------

/// A `GraphView` that makes an inner `NodeStoreView` accessible under a single prefix key.
///
/// `PrefixedGraphView::new("input", inner)` resolves `["input", "title"]`
/// by delegating `["title"]` to the inner view.
pub struct PrefixedGraphView<'a> {
    prefix: String,
    inner: NodeStoreView<'a>,
}

impl<'a> PrefixedGraphView<'a> {
    pub fn new(prefix: String, inner: NodeStoreView<'a>) -> Self {
        Self { prefix, inner }
    }
}

impl GraphView for PrefixedGraphView<'_> {
    fn resolve(&self, path: &[&str]) -> Option<DataRef<'_>> {
        match path {
            [] => None,
            [first, rest @ ..] if *first == self.prefix => {
                if rest.is_empty() {
                    // Resolve the prefix itself: materialise the root as a Value
                    Some(DataRef::Owned(node_to_value(
                        self.inner.store(),
                        self.inner.root(),
                    )))
                } else {
                    self.inner.resolve(rest)
                }
            }
            _ => None,
        }
    }

    fn iter_keys(&self) -> Vec<String> {
        vec![self.prefix.clone()]
    }

    fn clone_scoped(&self, path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
        match path {
            [first] if *first == self.prefix => Some(Box::new(NodeStoreView::new(
                self.inner.store(),
                self.inner.root(),
            ))),
            [first, rest @ ..] if *first == self.prefix => self.inner.clone_scoped(rest),
            _ => None,
        }
    }

    fn resolve_node(&self, path: &[&str]) -> Option<ResolvedNode<'_>> {
        match path {
            [] => None,
            [first, rest @ ..] if *first == self.prefix => {
                if rest.is_empty() {
                    // The prefix itself — return the root node
                    Some(ResolvedNode { id: self.inner.root(), store: self.inner.store() })
                } else {
                    self.inner.resolve_node(rest)
                }
            }
            _ => None,
        }
    }

    fn with_binding(&self, key: String, value: Value) -> Box<dyn GraphView> {
        // Materialise inner root as a Value, wrap it under the prefix key,
        // then add the new binding.
        let root_value = node_to_value(self.inner.store(), self.inner.root());
        let mut graph = DataGraph::new();
        graph.insert(self.prefix.clone(), root_value);
        graph.insert(key, value);
        Box::new(graph)
    }

    fn with_node_binding(
        &self,
        key: String,
        id: NodeId,
        _store: &NodeStore,
    ) -> Option<Box<dyn GraphView + '_>> {
        // Convert to MultiRootView with the prefix root + the new binding.
        let roots = vec![(self.prefix.clone(), self.inner.root()), (key, id)];
        Some(Box::new(MultiRootView::new(self.inner.store(), roots)))
    }
}

// ---------------------------------------------------------------------------
// Store walking helper
// ---------------------------------------------------------------------------

/// Walk `path` segments through the store starting from `root`, returning the
/// terminal `NodeId`. Returns `None` if any segment is not found.
///
/// This mirrors the walk logic in `NodeStoreView::resolve` but is a free
/// function so it can be called without creating a `NodeStoreView` temporary
/// (which would cause lifetime issues when the result borrows from `&self`).
fn walk_store(store: &NodeStore, root: NodeId, path: &[&str]) -> Option<NodeId> {
    let mut current = root;
    for segment in path {
        // ConsistsOf edges first
        let parts = store.consists_of(current);
        if let Some((_, id)) = parts.iter().find(|(n, _)| store.resolve_name(*n) == *segment) {
            current = *id;
            continue;
        }
        // Reference edges
        let refs = store.references(current);
        if let Some((_, id)) = refs.iter().find(|(n, _)| store.resolve_name(*n) == *segment) {
            current = *id;
            continue;
        }
        // Attribute edges
        let attrs = store.attributes(current);
        if let Some((_, id)) = attrs.iter().find(|(n, _)| store.resolve_name(*n) == *segment) {
            current = *id;
            continue;
        }
        return None;
    }
    Some(current)
}

// ---------------------------------------------------------------------------
// MultiRootView
// ---------------------------------------------------------------------------

/// A `GraphView` backed by multiple named roots in the same `NodeStore`.
///
/// All lookups walk the graph directly — no materialization unless the resolved
/// value is needed as a `DataRef`. Used by `data-each` to bind collection items
/// without allocating a `Value` per item.
pub struct MultiRootView<'a> {
    store: &'a NodeStore,
    roots: Vec<(String, NodeId)>,
}

impl<'a> MultiRootView<'a> {
    pub fn new(store: &'a NodeStore, roots: Vec<(String, NodeId)>) -> Self {
        Self { store, roots }
    }

    fn find_root(&self, key: &str) -> Option<NodeId> {
        self.roots.iter().find(|(k, _)| k == key).map(|(_, id)| *id)
    }
}

impl<'a> GraphView for MultiRootView<'a> {
    fn resolve(&self, path: &[&str]) -> Option<DataRef<'_>> {
        match path {
            [] => None,
            [key, rest @ ..] => {
                let root = self.find_root(key)?;
                // Walk the subpath inline — avoid temporary NodeStoreView borrow issues.
                let resolved_id = walk_store(self.store, root, rest)?;
                Some(DataRef::Owned(node_to_value(self.store, resolved_id)))
            }
        }
    }

    fn resolve_node(&self, path: &[&str]) -> Option<ResolvedNode<'_>> {
        match path {
            [] => None,
            [key, rest @ ..] => {
                let root = self.find_root(key)?;
                if rest.is_empty() {
                    Some(ResolvedNode { id: root, store: self.store })
                } else {
                    let id = walk_store(self.store, root, rest)?;
                    Some(ResolvedNode { id, store: self.store })
                }
            }
        }
    }

    fn iter_keys(&self) -> Vec<String> {
        self.roots.iter().map(|(k, _)| k.clone()).collect()
    }

    fn clone_scoped(&self, path: &[&str]) -> Option<Box<dyn GraphView + '_>> {
        match path {
            [key] => {
                let root = self.find_root(key)?;
                Some(Box::new(NodeStoreView::new(self.store, root)))
            }
            [key, rest @ ..] => {
                let root = self.find_root(key)?;
                let scoped_root = walk_store(self.store, root, rest)?;
                Some(Box::new(NodeStoreView::new(self.store, scoped_root)))
            }
            _ => None,
        }
    }

    fn with_binding(&self, key: String, value: Value) -> Box<dyn GraphView> {
        // Materialize all roots into a DataGraph, then add the new binding.
        let mut graph = DataGraph::new();
        for (k, root_id) in &self.roots {
            graph.insert(k.as_str(), node_to_value(self.store, *root_id));
        }
        graph.insert(key, value);
        Box::new(graph)
    }

    fn with_node_binding(
        &self,
        key: String,
        id: NodeId,
        _store: &NodeStore,
    ) -> Option<Box<dyn GraphView + '_>> {
        let mut new_roots = self.roots.clone();
        // Replace existing binding or append a new one.
        if let Some(pos) = new_roots.iter().position(|(k, _)| k == &key) {
            new_roots[pos].1 = id;
        } else {
            new_roots.push((key, id));
        }
        Some(Box::new(MultiRootView::new(self.store, new_roots)))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use node_store::{Edge, Node, NodeStore};
    use template::data::Value;
    use template::graph_view::GraphView;
    use template;

    fn make_test_store() -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root = store.add_node(Node::Element(page_name));

        let title_name = store.intern("title");
        let title_val = store.add_node(Node::Text("Hello World".into()));
        store.add_edge(
            root,
            Edge::Reference {
                name: title_name,
                target: title_val,
            },
        );

        // Nested: author record with name field
        let author_name = store.intern("author");
        let author_elem_name = store.intern("record");
        let author = store.add_node(Node::Element(author_elem_name));
        let name_key = store.intern("name");
        let name_val = store.add_node(Node::Text("Alice".into()));
        store.add_edge(
            author,
            Edge::Reference {
                name: name_key,
                target: name_val,
            },
        );
        store.add_edge(
            root,
            Edge::Reference {
                name: author_name,
                target: author,
            },
        );

        (store, root)
    }

    #[test]
    fn resolve_simple_text() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let result = view.resolve(&["title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Hello World"));
    }

    #[test]
    fn resolve_nested_path() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let result = view.resolve(&["author", "name"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Alice"));
    }

    #[test]
    fn resolve_missing_returns_none() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        assert!(view.resolve(&["missing"]).is_none());
    }

    #[test]
    fn iter_keys_returns_reference_names() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let keys = view.iter_keys();
        assert!(keys.contains(&"title".to_string()));
        assert!(keys.contains(&"author".to_string()));
    }

    #[test]
    fn clone_scoped_narrows_root() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let scoped = view.clone_scoped(&["author"]).unwrap();
        let result = scoped.resolve(&["name"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Alice"));
    }

    #[test]
    fn with_binding_adds_key() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let bound = view.with_binding("extra".into(), Value::Text("bonus".into()));
        assert!(bound.resolve(&["extra"]).is_some());
        // Original keys still accessible
        assert!(bound.resolve(&["title"]).is_some());
    }

    #[test]
    fn layered_view_local_overrides_parent() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let bound = view.with_binding("title".into(), Value::Text("Overridden".into()));
        let result = bound.resolve(&["title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Overridden"));
    }

    #[test]
    fn layered_view_iter_keys_includes_both() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let bound = view.with_binding("extra".into(), Value::Text("x".into()));
        let keys = bound.iter_keys();
        assert!(keys.contains(&"title".to_string()));
        assert!(keys.contains(&"extra".to_string()));
    }

    #[test]
    fn prefixed_resolve_with_matching_prefix() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        let result = prefixed.resolve(&["input", "title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Hello World"));
    }

    #[test]
    fn prefixed_resolve_wrong_prefix() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        assert!(prefixed.resolve(&["other", "title"]).is_none());
    }

    #[test]
    fn prefixed_resolve_deep_path() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        let result = prefixed.resolve(&["input", "author", "name"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Alice"));
    }

    #[test]
    fn prefixed_iter_keys() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        assert_eq!(prefixed.iter_keys(), vec!["input"]);
    }

    #[test]
    fn prefixed_clone_scoped_at_prefix() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        let scoped = prefixed.clone_scoped(&["input"]).unwrap();
        let result = scoped.resolve(&["title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Hello World"));
    }

    #[test]
    fn prefixed_with_binding_preserves_prefix() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        let bound = prefixed.with_binding("extra".into(), Value::Text("bonus".into()));
        assert!(bound.resolve(&["input", "title"]).is_some());
        assert!(bound.resolve(&["extra"]).is_some());
    }

    #[test]
    fn resolve_follows_consists_of_edges() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root = store.add_node(Node::Element(page_name));

        let title_name = store.intern("title");
        let title_val = store.add_node(Node::Text("Via ConsistsOf".into()));
        store.add_edge(root, Edge::ConsistsOf { name: title_name, part: title_val });

        let view = NodeStoreView::new(&store, root);
        let result = view.resolve(&["title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Via ConsistsOf"));
    }

    #[test]
    fn iter_keys_includes_consists_of_names() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root = store.add_node(Node::Element(page_name));

        let title_name = store.intern("title");
        let title_val = store.add_node(Node::Text("Hello".into()));
        store.add_edge(root, Edge::ConsistsOf { name: title_name, part: title_val });

        let author_name = store.intern("author");
        let author_val = store.add_node(Node::Text("Alice".into()));
        store.add_edge(root, Edge::Reference { name: author_name, target: author_val });

        let view = NodeStoreView::new(&store, root);
        let keys = view.iter_keys();
        assert!(keys.contains(&"title".to_string()));
        assert!(keys.contains(&"author".to_string()));
    }

    #[test]
    fn resolve_node_returns_node_id() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let resolved = view.resolve_node(&["title"]).unwrap();
        assert!(matches!(store.get(resolved.id), Some(Node::Text(t)) if t == "Hello World"));
    }

    #[test]
    fn resolve_node_nested_path() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let resolved = view.resolve_node(&["author", "name"]).unwrap();
        assert!(matches!(store.get(resolved.id), Some(Node::Text(t)) if t == "Alice"));
    }

    #[test]
    fn resolve_node_missing_returns_none() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        assert!(view.resolve_node(&["missing"]).is_none());
    }

    #[test]
    fn prefixed_resolve_node() {
        let (store, root) = make_test_store();
        let view = NodeStoreView::new(&store, root);
        let prefixed = PrefixedGraphView::new("input".to_string(), view);
        let resolved = prefixed.resolve_node(&["input", "title"]).unwrap();
        assert!(matches!(store.get(resolved.id), Some(Node::Text(t)) if t == "Hello World"));
    }

    #[test]
    fn datagraph_resolve_node_returns_none() {
        // DataGraph doesn't support resolve_node — returns None
        let mut g = template::DataGraph::new();
        g.insert("title", template::Value::Text("Hello".into()));
        let view: &dyn template::GraphView = &g;
        assert!(view.resolve_node(&["title"]).is_none());
    }

    #[test]
    fn multi_root_view_resolves_multiple_roots() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root1 = store.add_node(Node::Element(page_name));
        let root2 = store.add_node(Node::Element(page_name));
        let title_name = store.intern("title");
        let t1 = store.add_node(Node::Text("Page One".into()));
        let t2 = store.add_node(Node::Text("Item Title".into()));
        store.add_edge(root1, Edge::Reference { name: title_name, target: t1 });
        store.add_edge(root2, Edge::Reference { name: title_name, target: t2 });

        let view = MultiRootView::new(&store, vec![
            ("input".to_string(), root1),
            ("item".to_string(), root2),
        ]);

        // Resolve input.title
        let r1 = view.resolve_node(&["input", "title"]).unwrap();
        assert!(matches!(store.get(r1.id), Some(Node::Text(t)) if t == "Page One"));

        // Resolve item.title
        let r2 = view.resolve_node(&["item", "title"]).unwrap();
        assert!(matches!(store.get(r2.id), Some(Node::Text(t)) if t == "Item Title"));
    }

    #[test]
    fn multi_root_view_resolve_returns_owned_value() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root = store.add_node(Node::Element(page_name));
        let title_name = store.intern("title");
        let title_val = store.add_node(Node::Text("Hello".into()));
        store.add_edge(root, Edge::Reference { name: title_name, target: title_val });

        let view = MultiRootView::new(&store, vec![("input".to_string(), root)]);
        let result = view.resolve(&["input", "title"]).unwrap();
        assert!(matches!(result.as_value(), Value::Text(t) if t == "Hello"));
    }

    #[test]
    fn multi_root_view_with_node_binding_adds_root() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root1 = store.add_node(Node::Element(page_name));
        let root2 = store.add_node(Node::Element(page_name));
        let title_name = store.intern("title");
        let t1 = store.add_node(Node::Text("Original".into()));
        let t2 = store.add_node(Node::Text("Bound".into()));
        store.add_edge(root1, Edge::Reference { name: title_name, target: t1 });
        store.add_edge(root2, Edge::Reference { name: title_name, target: t2 });

        let view = MultiRootView::new(&store, vec![("input".to_string(), root1)]);
        let bound = view.with_node_binding("item".to_string(), root2, &store).unwrap();

        // Bound key resolves to new root
        let r1 = bound.resolve_node(&["item", "title"]).unwrap();
        assert!(matches!(store.get(r1.id), Some(Node::Text(t)) if t == "Bound"));

        // Original key still accessible
        let r2 = bound.resolve_node(&["input", "title"]).unwrap();
        assert!(matches!(store.get(r2.id), Some(Node::Text(t)) if t == "Original"));
    }

    #[test]
    fn prefixed_with_node_binding_creates_multi_root() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root1 = store.add_node(Node::Element(page_name));
        let root2 = store.add_node(Node::Element(page_name));
        let title_name = store.intern("title");
        let t1 = store.add_node(Node::Text("Page".into()));
        let t2 = store.add_node(Node::Text("Item".into()));
        store.add_edge(root1, Edge::Reference { name: title_name, target: t1 });
        store.add_edge(root2, Edge::Reference { name: title_name, target: t2 });

        let inner = NodeStoreView::new(&store, root1);
        let prefixed = PrefixedGraphView::new("input".to_string(), inner);

        let bound = prefixed.with_node_binding("item".to_string(), root2, &store).unwrap();

        // Both paths resolve
        let r1 = bound.resolve_node(&["input", "title"]).unwrap();
        assert!(matches!(store.get(r1.id), Some(Node::Text(t)) if t == "Page"));
        let r2 = bound.resolve_node(&["item", "title"]).unwrap();
        assert!(matches!(store.get(r2.id), Some(Node::Text(t)) if t == "Item"));
    }

    #[test]
    fn multi_root_view_iter_keys() {
        let mut store = NodeStore::new();
        let page_name = store.intern("page");
        let root = store.add_node(Node::Element(page_name));
        let view = MultiRootView::new(&store, vec![
            ("input".to_string(), root),
            ("item".to_string(), root),
        ]);
        let keys = view.iter_keys();
        assert!(keys.contains(&"input".to_string()));
        assert!(keys.contains(&"item".to_string()));
    }
}
