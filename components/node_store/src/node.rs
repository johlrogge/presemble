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
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// Named structural element
    Element(Name),
    /// Raw text content
    Text(String),
    /// EDN keyword
    Keyword(Name),
    /// Integer value
    Integer(i64),
    /// Boolean value
    Boolean(bool),
    /// Nil / absent
    Nil,
}
