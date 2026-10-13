use std::fmt;

#[derive(Debug)]
pub enum CliError {
    Schema(schema::SchemaError),
    Content(content::ContentError),
    Io(std::io::Error),
    Usage(String),
    Template(template::RenderError),
    Render(String),
    SessionKey(accounts::SessionKeyError),
    /// The publish gate (ADR-061): the site has validation errors, so nothing
    /// was written.
    Unpublishable {
        errors: usize,
        files: usize,
    },
    /// Pages that failed to render during a publish build.
    PagesFailed(usize),
    /// `presemble fmt --check`: files that are not in their canonical form.
    NotFormatted(usize),
    /// `presemble fmt`: files that could not be formatted.
    FormatFailed(usize),
}

impl CliError {
    /// The process exit code for this error: 2 for files `fmt` could not
    /// format, 1 for everything else (including `fmt --check` finding
    /// changes).
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::FormatFailed(_) => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Schema(e) => write!(f, "schema error: {e}"),
            CliError::Content(e) => write!(f, "content error: {e}"),
            CliError::Io(e) => write!(f, "io error: {e}"),
            CliError::Usage(msg) => write!(f, "usage: {msg}"),
            CliError::Template(e) => write!(f, "template error: {e}"),
            CliError::Render(msg) => write!(f, "render error: {msg}"),
            CliError::SessionKey(e) => write!(f, "{e}"),
            CliError::Unpublishable { errors, files } => write!(
                f,
                "{errors} validation error(s) in {files} file(s); site not published"
            ),
            CliError::PagesFailed(n) => write!(f, "{n} page(s) failed to render"),
            CliError::NotFormatted(n) => write!(f, "{n} file(s) would be reformatted"),
            CliError::FormatFailed(n) => write!(f, "{n} file(s) could not be formatted"),
        }
    }
}

impl From<accounts::SessionKeyError> for CliError {
    fn from(e: accounts::SessionKeyError) -> Self {
        CliError::SessionKey(e)
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

impl From<template::RenderError> for CliError {
    fn from(e: template::RenderError) -> Self {
        CliError::Template(e)
    }
}
