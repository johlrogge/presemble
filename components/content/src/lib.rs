mod document;
mod error;
mod parser;
mod serializer;
mod slot_assignment;
mod slot_editor;
mod validator;

pub use document::{ContentElement, Document, DocumentSlot, FlatDocument};
pub use error::ContentError;
pub use parser::{byte_to_position, parse_and_assign, parse_document};
pub use serializer::serialize_document;
pub use slot_assignment::assign_slots;
pub use slot_editor::{capitalize_slot, modify_slot};
pub use validator::{validate, Severity, ValidationDiagnostic, ValidationResult};
