use crate::document::Document;
use schema::{Grammar, SlotName};

/// The result of validating a document against a grammar.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub diagnostics: Vec<ValidationDiagnostic>,
}

impl ValidationResult {
    /// Returns `true` if no errors were found (warnings are acceptable).
    pub fn is_valid(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|d| !matches!(d.severity, Severity::Error))
    }
}

/// A single validation finding.
#[derive(Debug, Clone)]
pub struct ValidationDiagnostic {
    pub severity: Severity,
    pub message: String,
    pub slot: Option<SlotName>,
}

/// Severity level for a validation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// Validate a content document against a schema grammar.
///
/// Checks that the document's structure matches the grammar's preamble slots
/// and body rules, returning all diagnostics found.
pub fn validate(_doc: &Document, _grammar: &Grammar) -> ValidationResult {
    todo!("validator not yet implemented")
}
