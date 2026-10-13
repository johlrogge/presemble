//! The site directory a command works on, checked before anything is started
//! for it: a command given a wrong path says so at once, naming the path,
//! rather than starting a conductor that cannot run.

use crate::error::CliError;
use std::path::{Path, PathBuf};

/// A site directory that exists (and, for [`SiteDir::with_schemas`], has a
/// `schemas/` directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteDir(PathBuf);

impl SiteDir {
    /// A directory that exists: enough for the conductor, which also serves a
    /// site that is still empty (the welcome page, the site wizard).
    pub fn existing(path: &Path) -> Result<SiteDir, CliError> {
        if !path.exists() {
            return Err(CliError::SiteDir(format!(
                "the site directory {} does not exist",
                path.display()
            )));
        }
        if !path.is_dir() {
            return Err(CliError::SiteDir(format!(
                "{} is not a directory: give the site directory",
                path.display()
            )));
        }
        Ok(SiteDir(path.to_path_buf()))
    }

    /// A directory that exists and has `schemas/`: a site an editor can work
    /// on.
    pub fn with_schemas(path: &Path) -> Result<SiteDir, CliError> {
        let site = SiteDir::existing(path)?;
        if !path.join("schemas").is_dir() {
            return Err(CliError::SiteDir(format!(
                "{} is not a site directory: it has no schemas/",
                path.display()
            )));
        }
        Ok(site)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(result: Result<SiteDir, CliError>) -> String {
        result.expect_err("refused").to_string()
    }

    #[test]
    fn a_missing_directory_is_an_error_that_names_it() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("demo");
        let started = std::time::Instant::now();

        for check in [SiteDir::existing, SiteDir::with_schemas] {
            let error = message(check(&missing));
            assert!(error.contains(&missing.display().to_string()), "{error}");
            assert!(error.contains("does not exist"), "{error}");
        }
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "at once, without starting anything"
        );
    }

    #[test]
    fn a_file_is_not_a_site_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("site.md");
        std::fs::write(&file, "# not a site\n").unwrap();
        let error = message(SiteDir::existing(&file));
        assert!(error.contains(&file.display().to_string()), "{error}");
        assert!(error.contains("is not a directory"), "{error}");
    }

    #[test]
    fn an_editor_needs_a_schemas_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let error = message(SiteDir::with_schemas(tmp.path()));
        assert!(error.contains(&tmp.path().display().to_string()), "{error}");
        assert!(error.contains("schemas/"), "{error}");

        // The conductor serves an empty site too.
        assert_eq!(
            SiteDir::existing(tmp.path())
                .map(|s| s.path().to_path_buf())
                .ok(),
            Some(tmp.path().to_path_buf())
        );
        std::fs::create_dir(tmp.path().join("schemas")).unwrap();
        assert!(SiteDir::with_schemas(tmp.path()).is_ok());
    }
}
