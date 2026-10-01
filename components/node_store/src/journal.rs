use im::OrdSet;

use crate::node::NodeId;

/// Mutation journal recorded by a [`crate::NodeStore`] while a journal is active.
///
/// Records the net change to the store since
/// [`crate::NodeStore::start_journal`] was called, as three sets:
///
/// - `created`: nodes created within the journal that are still alive.
/// - `removed`: nodes that existed before the journal started and are now gone.
/// - `touched`: nodes that existed before the journal started, are still
///   alive, and had their value or outgoing edges changed. This includes
///   sources of any edge (Child, Attribute, Reference, ConsistsOf, Derived)
///   into a node that was removed.
///
/// Invariant: the three sets never overlap. A node created and then removed
/// within the same journal appears in none of them.
///
/// The sets are persistent (`im`), so cloning a store with an active journal
/// is O(1) and the clone's journal evolves independently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TouchLog {
    /// New nodes that are still alive.
    pub created: OrdSet<NodeId>,
    /// Pre-existing, still-alive nodes whose value or outgoing edges changed.
    pub touched: OrdSet<NodeId>,
    /// Pre-existing nodes that are now gone.
    pub removed: OrdSet<NodeId>,
}

impl TouchLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// True when nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.touched.is_empty() && self.removed.is_empty()
    }
}
