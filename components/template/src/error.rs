use std::fmt;

#[derive(Debug)]
pub enum TemplateError {
    ParseError(String),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::ParseError(msg) => write!(f, "template parse error: {msg}"),
        }
    }
}

impl std::error::Error for TemplateError {}
