mod lsp_capabilities;

pub use lsp_capabilities::{
    completions_for_schema, hover_for_line, validate_with_positions, CapitalizationFix,
    DiagnosticSeverity, PositionedDiagnostic, SlotCompletion,
};
