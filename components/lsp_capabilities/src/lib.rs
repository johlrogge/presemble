mod lsp_capabilities;

pub use lsp_capabilities::{
    completions_for_schema, definition_for_position, hover_for_line, validate_with_positions,
    CapitalizationFix, DiagnosticSeverity, PositionedDiagnostic, SlotCompletion, TemplateFix,
};
