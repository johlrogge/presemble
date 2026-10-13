mod adapters;
pub mod address;
mod block;
mod diff;
mod document;
mod error;
mod format;
pub mod html;
pub mod inline;
mod link_expression;
mod locate;
mod parser;
mod serializer;
mod shape;
mod slot_assignment;
pub mod slot_editor;
pub mod sync;
mod transform;
pub mod unfinished;
mod validator;

pub use adapters::{
    DomPatch, FileWriter, FullDocumentWriter, SourceEdit, diff_to_dom_patches, diff_to_source_edits,
};
pub use address::{AddressContainer, block_addresses, clj_str};
pub use block::{
    Projection, RoundTripError, check_block_round_trip, parse_markdown_projection, project_markdown,
};
pub use diff::{Change, DocumentDiff, diff};
pub use document::{
    Align, AnchoredComment, Blocks, Column, Comment, CommentAnchor, Container, ContentElement,
    Document, DocumentSlot, FlatDocument, Keyword, KeywordError, LinkOp, LinkTarget, LinkText,
    List, ListKind, RefsToTarget, SlotMarker, SortDirection, Spacing,
};
pub use error::ContentError;
pub use format::format_document;
pub use inline::AsPlainText;
pub use link_expression::{
    Candidate, LinkExpressionDiagnostic, LinkExpressionError, diagnose_paragraph, parse_thread,
    recognise_paragraph,
};
pub use parser::{
    MARKDOWN_OPTIONS, byte_to_position, parse_and_assign, parse_document, parse_link_target,
    parse_link_text,
};
pub use serializer::serialize_document;
pub use shape::{BlockShape, ImageParts, LinkParts, SoleImage, SoleLink};
pub use slot_assignment::{UnassignedReason, assign_slots, unassigned_reason};
pub use transform::{
    Capitalize, CompositeTransform, InsertSeparator, InsertSlot, Transform, TransformError,
};
pub use unfinished::{Hit, Unfinished, unfinished_markup};
pub use validator::{
    Location, NestedStep, Severity, ValidationDiagnostic, ValidationResult, span_of_location,
    validate, validate_source,
};
