// 5b.2: `ContentElement::Link` and `Image` are deprecated and removed in 5b.2;
// until then this crate still reads their `link` and `image` store kinds.
// 5b.2: remove this allow
#![allow(deprecated)]

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
pub mod block_edit;

pub use body_fragment::parse_body_fragment;
pub use schema_document::synthesize_schema_document;
pub use content_bridge::{
    BLOCK_KINDS, BlockDecodeError, DocumentDecodeError, DocumentMeta, PAYLOAD_BLOCK_KINDS,
    TEXT_BLOCK_KINDS, block_from_node, block_to_store, block_to_tree, owning_block, owning_document,
    presemble_file_for_root, serialize_from_store, store_to_document, tree_to_block,
};
pub use value_bridge::{node_to_value, value_to_node};
pub use graph_view_impl::{LayeredGraphView, MultiRootView, NodeStoreView, PrefixedGraphView};
pub use slot_edit::modify_slot_in_store;
