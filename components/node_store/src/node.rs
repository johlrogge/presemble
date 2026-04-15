/// Opaque internal identifier. Never serialized, never in NED programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) u64);

/// Interned name identifier for fast comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameId(pub(crate) u32);

/// A name in the node graph — element names, attribute names, edge labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub(crate) NameId);

/// A node in the DAG. Nodes are pure values — all relationships live in edges.
#[derive(Debug, Clone)]
pub enum Node {
    /// Named structural element — has Attribute/Reference/ConsistsOf/Child edges
    Element(Name),
    /// Raw text content
    Text(String),
    /// Ordered sequence — Child edges are its items. No name.
    Collection,
    /// EDN keyword
    Keyword(Name),
    /// Integer value
    Integer(i64),
    /// Boolean value
    Boolean(bool),
    /// Nil / absent
    Nil,
    /// Opaque Rust value — closures, handles. Not serializable.
    Opaque(std::sync::Arc<dyn std::any::Any + Send + Sync>),
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Node::Element(a), Node::Element(b)) => a == b,
            (Node::Text(a), Node::Text(b)) => a == b,
            (Node::Collection, Node::Collection) => true,
            (Node::Keyword(a), Node::Keyword(b)) => a == b,
            (Node::Integer(a), Node::Integer(b)) => a == b,
            (Node::Boolean(a), Node::Boolean(b)) => a == b,
            (Node::Nil, Node::Nil) => true,
            (Node::Opaque(a), Node::Opaque(b)) => std::sync::Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
