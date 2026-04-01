mod error;
mod grammar;
mod parser;
pub mod span;

pub use error::SchemaError;
pub use grammar::{
    AltRequirement, BodyRules, Constraint, ContentConstraint, CountRange, Element, Grammar,
    HeadingLevel, HeadingLevelRange, Orientation, Slot, SlotName,
};
pub use parser::parse_schema;
pub use span::{Span, Spanned};
