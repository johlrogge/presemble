mod diff;
mod document;
mod error;
mod parser;
mod serializer;
mod slot_assignment;
mod slot_editor;
mod transform;
mod validator;

pub use diff::{diff, Change, DocumentDiff};
pub use document::{ContentElement, Document, DocumentSlot, FlatDocument};
pub use error::ContentError;
pub use parser::{byte_to_position, parse_and_assign, parse_document};
pub use serializer::serialize_document;
pub use slot_assignment::assign_slots;
pub use transform::{
    Capitalize, CompositeTransform, InsertSeparator, InsertSlot, Transform, TransformError,
};
pub use validator::{validate, Severity, ValidationDiagnostic, ValidationResult};
