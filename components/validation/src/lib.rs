mod validation;

pub use validation::{
    Diagnostic, LineCol, Severity, SourceDiagnostic, ValidationReport, line_col, validate_content,
    validate_document, validate_schema, validate_template,
};
