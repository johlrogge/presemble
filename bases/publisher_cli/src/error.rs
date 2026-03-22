use std::fmt;

#[derive(Debug)]
pub enum CliError {
    Schema(schema::SchemaError),
    Content(content::ContentError),
    Io(std::io::Error),
    Usage(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Schema(e) => write!(f, "schema error: {e}"),
            CliError::Content(e) => write!(f, "content error: {e}"),
            CliError::Io(e) => write!(f, "io error: {e}"),
            CliError::Usage(msg) => write!(f, "usage: {msg}"),
        }
    }
}

impl From<schema::SchemaError> for CliError {
    fn from(e: schema::SchemaError) -> Self {
        CliError::Schema(e)
    }
}

impl From<content::ContentError> for CliError {
    fn from(e: content::ContentError) -> Self {
        CliError::Content(e)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::Io(e)
    }
}
