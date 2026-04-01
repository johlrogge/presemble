mod lsp_capabilities;

pub use lsp_capabilities::{
    apply_action, build_transform,
    completions_for_schema, content_completions, definition_for_position, hover_for_line,
    link_completions,
    template_completions, schema_completions, template_definition, validate_schema_with_positions,
    validate_template_paths,
    validate_with_positions, write_slot_to_file, write_slot_to_string,
    DiagnosticSeverity, PositionedDiagnostic,
    SlotAction, SlotCompletion, TemplateDefinitionTarget,
};
