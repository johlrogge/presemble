use std::fmt;

/// Errors that can occur when parsing a content document.
#[derive(Debug)]
pub enum ContentError {
    /// The input could not be parsed as a valid content document.
    ParseError(String),
}

impl fmt::Display for ContentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentError::ParseError(msg) => write!(f, "content parse error: {msg}"),
        }
    }
}

impl std::error::Error for ContentError {}
