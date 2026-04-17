use crate::node::{Name, NodeId};

/// A directed relationship between nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edge {
    /// Ordered child containment. Position in the edge vector determines order.
    Child(NodeId),
    /// Named attribute: element --[:name]--> value
    Attribute { name: Name, value: NodeId },
    /// Cross-document reference (can be circular)
    Reference { name: Name, target: NodeId },
    /// Structural composition: page consists of header, nav, body, footer.
    /// Distinct from Reference (link TO) — ConsistsOf is a build dependency.
    ConsistsOf { name: Name, part: NodeId },
    /// Computed relationship (ephemeral, never serialized)
    Derived { name: Name, target: NodeId },
}

impl Edge {
    /// Returns the target node for any edge variant.
    pub fn target(&self) -> NodeId {
        match self {
            Edge::Child(id) => *id,
            Edge::Attribute { value, .. } => *value,
            Edge::Reference { target, .. } => *target,
            Edge::ConsistsOf { part, .. } => *part,
            Edge::Derived { target, .. } => *target,
        }
    }
}
