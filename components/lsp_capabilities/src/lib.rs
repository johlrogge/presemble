mod lsp_capabilities;

pub use lsp_capabilities::{
    completions_for_schema, content_completions, definition_for_position, hover_for_line,
    template_completions, schema_completions, template_definition, validate_schema_with_positions,
    validate_template_paths,
    validate_with_positions, CapitalizationFix, DiagnosticSeverity, PositionedDiagnostic,
    SlotCompletion, TemplateFix, TemplateDefinitionTarget,
};
