mod adapters;
pub mod address;
mod block;
mod diff;
mod document;
mod error;
pub mod html;
pub mod inline;
mod link_expression;
mod parser;
mod serializer;
mod shape;
mod slot_assignment;
pub mod slot_editor;
pub mod sync;
mod transform;
mod validator;

pub use adapters::{
    diff_to_dom_patches, diff_to_source_edits, DomPatch, FileWriter, FullDocumentWriter,
    SourceEdit,
};
pub use address::{block_addresses, clj_str, AddressContainer};
pub use block::{check_block_round_trip, parse_markdown_projection, project_markdown, Projection, RoundTripError};
pub use diff::{diff, Change, DocumentDiff};
pub use document::{
    Align, AnchoredComment, Blocks, Column, Comment, CommentAnchor, Container, ContentElement, Document, DocumentSlot,
    FlatDocument, Keyword, KeywordError, LinkOp, LinkTarget, LinkText, List, ListKind, RefsToTarget,
    SlotMarker, SortDirection, Spacing,
};
pub use error::ContentError;
pub use inline::AsPlainText;
pub use link_expression::{
    diagnose_paragraph, parse_thread, recognise_paragraph, Candidate, LinkExpressionDiagnostic,
    LinkExpressionError,
};
pub use parser::{
    byte_to_position, parse_and_assign, parse_document, parse_link_target, parse_link_text,
    MARKDOWN_OPTIONS,
};
pub use serializer::serialize_document;
pub use shape::{BlockShape, ImageParts, LinkParts, SoleImage, SoleLink};
pub use slot_assignment::{assign_slots, unassigned_reason, UnassignedReason};
pub use transform::{
    Capitalize, CompositeTransform, InsertSeparator, InsertSlot, Transform, TransformError,
};
pub use validator::{validate, Severity, ValidationDiagnostic, ValidationResult};

