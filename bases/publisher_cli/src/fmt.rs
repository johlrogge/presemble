//! `presemble fmt [--check] <site-dir>` (ADR-070 decision 3): every file of a
//! kind that has a canonical writer is written exactly as a save would write
//! it. Today that is content; every other kind is left alone.

use std::path::{Path, PathBuf};

use site_index::{FileKind, SiteIndex};
use site_repository::SiteRepository;

use crate::CliError;

/// Why a file could not be formatted.
#[derive(Debug)]
pub enum FormatError {
    /// The file could not be read as text.
    Unreadable(String),
    /// The formatted text could not be written.
    Unwritable(std::io::Error),
    /// No schema describes the file.
    NoSchema(String),
    /// The schema exists but could not be read.
    Schema(String),
    /// The text could not be read as markdown.
    Content(content::ContentError),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::Unreadable(e) => write!(f, "cannot be read: {e}"),
            FormatError::Unwritable(e) => write!(f, "cannot be written: {e}"),
            FormatError::NoSchema(stem) => write!(f, "no schema for `{stem}`"),
            FormatError::Schema(e) => write!(f, "its schema cannot be read: {e}"),
            FormatError::Content(e) => write!(f, "{e}"),
        }
    }
}

/// How the text of a site file is written canonically: what a save of it
/// writes.
pub enum Formatter {
    /// A content file, read with the grammar of its page.
    Content(schema::Grammar),
}

impl Formatter {
    /// The canonical text of `text`.
    pub fn format(&self, text: &str) -> Result<String, FormatError> {
        match self {
            Formatter::Content(grammar) => {
                content::format_document(text, grammar).map_err(FormatError::Content)
            }
        }
    }
}

/// How a site file of this kind is formatted, if it is: for content, the
/// grammar its page uses (item or collection schema, as the conductor picks
/// it). `None` for kinds without a formatter; such files are not visited.
pub fn formatter_for(
    kind: &FileKind,
    path: &Path,
    site: &SiteRepository,
) -> Option<Result<Formatter, FormatError>> {
    match kind {
        FileKind::Content { schema_stem } => {
            Some(content_grammar(schema_stem, path, site).map(Formatter::Content))
        }
        _ => None,
    }
}

/// The grammar of a content file, as the conductor picks it: a collection's
/// `index.md` reads with the collection schema when there is one, every other
/// file with the item schema.
fn content_grammar(
    stem: &site_index::SchemaStem,
    path: &Path,
    site: &SiteRepository,
) -> Result<schema::Grammar, FormatError> {
    let is_index = path.file_stem().and_then(|s| s.to_str()) == Some("index");
    let source = is_index
        .then(|| site.collection_schema_source(stem))
        .flatten()
        .or_else(|| site.schema_source(stem))
        .ok_or_else(|| FormatError::NoSchema(stem.as_str().to_string()))?;
    schema::parse_schema(&source).map_err(|e| FormatError::Schema(e.to_string()))
}

/// What became of one file.
enum Outcome {
    Formatted,
    Canonical,
}

/// Format every file of `site_dir` that has a canonical writer. With `check`
/// nothing is written. Prints one line per file that changes (or would), each
/// error as `path: error: …`, then a summary.
pub fn run_fmt(site_dir: &Path, check: bool) -> Result<(), CliError> {
    let site_dir = std::fs::canonicalize(site_dir)?;
    let index = SiteIndex::new(site_dir.clone());
    let site = SiteRepository::builder().from_dir(&site_dir).build();

    let mut files = Vec::new();
    collect_files(&site_dir, &mut files)?;
    files.sort();

    let (mut formatted, mut canonical, mut errors) = (0usize, 0usize, 0usize);
    for path in files {
        let rel = path.strip_prefix(&site_dir).unwrap_or(&path).display();
        let Some(formatter) = formatter_for(&index.classify(&path), &path, &site) else {
            continue;
        };
        match formatter.and_then(|f| format_file(&f, &path, check)) {
            Ok(Outcome::Formatted) => {
                formatted += 1;
                if check {
                    println!("{rel}: would be reformatted");
                } else {
                    println!("{rel}");
                }
            }
            Ok(Outcome::Canonical) => canonical += 1,
            Err(e) => {
                errors += 1;
                eprintln!("{rel}: error: {e}");
            }
        }
    }

    let verb = if check {
        "would be reformatted"
    } else {
        "formatted"
    };
    println!("{formatted} files {verb}, {canonical} already canonical, {errors} errors");
    if errors > 0 {
        Err(CliError::FormatFailed(errors))
    } else if check && formatted > 0 {
        Err(CliError::NotFormatted(formatted))
    } else {
        Ok(())
    }
}

/// Read the file at `path`, format it with `formatter`, and write it back when
/// its text changes (never with `check`).
fn format_file(formatter: &Formatter, path: &Path, check: bool) -> Result<Outcome, FormatError> {
    let bytes = std::fs::read(path).map_err(|e| FormatError::Unreadable(e.to_string()))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| FormatError::Unreadable("not UTF-8 text".to_string()))?;
    let canonical = formatter.format(&text)?;
    if canonical == text {
        return Ok(Outcome::Canonical);
    }
    if !check {
        write_atomically(path, &canonical).map_err(FormatError::Unwritable)?;
    }
    Ok(Outcome::Formatted)
}

/// Write `text` to `path` through a temporary file in the same directory and a
/// rename, so the file is never seen half written.
fn write_atomically(path: &Path, text: &str) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    // The leading dot keeps a running serve from rebuilding on the temporary
    // file: its watcher ignores dot files (`is_relevant_path` in serve.rs).
    // Only the rename onto `path` is seen, as one change.
    let tmp = path.with_file_name(format!(".{name}.presemble-fmt.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Every file under `dir`, recursively.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}
