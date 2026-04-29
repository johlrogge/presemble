pub mod node_helpers;
pub mod schema_bridge;
pub mod schema_document;
pub mod content_bridge;
pub mod body_fragment;
pub mod template_bridge;
pub mod value_bridge;
pub mod store_pipeline;
pub mod graph_view_impl;
pub mod slot_edit;

pub use body_fragment::ingest_body_fragment;
pub use schema_document::synthesize_schema_document;
pub use content_bridge::{DocumentMeta, presemble_file_for_root, serialize_from_store};
pub use value_bridge::{node_to_value, value_to_node};
pub use graph_view_impl::{LayeredGraphView, MultiRootView, NodeStoreView, PrefixedGraphView};
pub use slot_edit::modify_slot_in_store;
