use crate::error::SchemaError;
use crate::grammar::Grammar;

/// Parse annotated markdown into a document grammar.
///
/// The input should follow the schema format described in ADR-001:
/// annotated markdown with `{#name}` anchors and definition list constraints.
pub fn parse_schema(_input: &str) -> Result<Grammar, SchemaError> {
    todo!("schema parser not yet implemented")
}
