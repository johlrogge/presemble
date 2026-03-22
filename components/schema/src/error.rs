use std::fmt;

/// Errors that can occur when parsing a schema file.
#[derive(Debug)]
pub enum SchemaError {
    /// The input was not valid schema markdown.
    ParseError(String),
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaError::ParseError(msg) => write!(f, "schema parse error: {msg}"),
        }
    }
}

impl std::error::Error for SchemaError {}
