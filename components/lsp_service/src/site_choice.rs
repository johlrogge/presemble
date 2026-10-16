//! Which site a language server serves, and where a file stands with it. One
//! site per language server process (D-S1); the server learns its site from
//! the command line, the workspace root, or the first file opened.

use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

/// The site a language server serves, once it knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteChoice {
    /// Always an absolute path.
    Chosen(PathBuf),
    /// Nothing known yet says which site: wait for a file of one.
    NotYet,
}

/// Choose the site from, in order: the command-line argument, the workspace
/// root if it is in a site, the first file opened if it is in one.
pub fn choose_site(arg: Option<&Path>, root: Option<&Path>, opened: Option<&Path>) -> SiteChoice {
    if let Some(arg) = arg {
        return SiteChoice::Chosen(absolute(arg));
    }
    root.and_then(site_index::site_of)
        .or_else(|| opened.and_then(site_index::site_of))
        .map_or(SiteChoice::NotYet, SiteChoice::Chosen)
}

/// `path` resolved against the working directory, canonical when it can be.
fn absolute(path: &Path) -> PathBuf {
    let joined = std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf());
    joined.canonicalize().unwrap_or(joined)
}

/// Where a file stands with the site a server serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePlace {
    /// It is a file of the served site.
    Belongs,
    /// It is a file of this other site.
    OtherSite(PathBuf),
    /// It is in no site at all (a README, an ADR).
    NoSite,
}

/// Where `file` stands with `site` (absolute, as a [`SiteChoice::Chosen`]).
pub fn file_in_session(site: &Path, file: &Path) -> FilePlace {
    match site_index::site_of(file) {
        None => FilePlace::NoSite,
        Some(own) if own == site => FilePlace::Belongs,
        Some(own) => FilePlace::OtherSite(own),
    }
}

/// What a file of site `file_site` is told by a server of site `served`: one
/// Information diagnostic at line 1, editor-neutral (the browser and other
/// editors use this server too).
pub fn other_site_diagnostic(file_site: &Path, served: &Path) -> Diagnostic {
    Diagnostic {
        range: Range::new(Position::new(0, 0), Position::new(0, 0)),
        severity: Some(DiagnosticSeverity::INFORMATION),
        source: Some("presemble".to_string()),
        message: format!(
            "This file belongs to site `{}`, but this language server serves `{}`. Save it, \
             then close and reopen it, to give it a language server for its own site.",
            file_site.display(),
            served.display()
        ),
        ..Diagnostic::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir holding sites, so nothing a test makes lands outside it.
    struct Tree {
        tmp: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Self {
            Tree {
                tmp: tempfile::tempdir().unwrap(),
            }
        }

        /// A site (a directory with `schemas/`) at `rel`, canonical.
        fn site(&self, rel: &str) -> PathBuf {
            let root = self.tmp.path().join(rel);
            std::fs::create_dir_all(root.join("schemas")).unwrap();
            root.canonicalize().unwrap()
        }

        fn file(&self, rel: &str) -> PathBuf {
            let path = self.tmp.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "x\n").unwrap();
            path
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.tmp.path().join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    #[test]
    fn the_argument_wins_over_a_root_and_an_opened_file() {
        let tree = Tree::new();
        let a = tree.site("a");
        let b = tree.site("b");
        tree.site("c");
        let opened = tree.file("c/content/x.md");
        assert_eq!(
            choose_site(Some(&a), Some(&b), Some(&opened)),
            SiteChoice::Chosen(a)
        );
    }

    #[test]
    fn a_root_that_is_a_site_wins_over_an_opened_file() {
        let tree = Tree::new();
        let b = tree.site("b");
        tree.site("c");
        let opened = tree.file("c/content/x.md");
        assert_eq!(
            choose_site(None, Some(&b), Some(&opened)),
            SiteChoice::Chosen(b)
        );
    }

    #[test]
    fn a_root_that_is_no_site_falls_through_to_the_opened_file() {
        let tree = Tree::new();
        let repo = tree.dir("repo");
        let site = tree.site("repo/site");
        let opened = tree.file("repo/site/content/post/a.md");
        assert_eq!(
            choose_site(None, Some(&repo), Some(&opened)),
            SiteChoice::Chosen(site)
        );
    }

    #[test]
    fn nothing_known_is_not_yet() {
        let tree = Tree::new();
        let repo = tree.dir("repo");
        let readme = tree.file("repo/README.md");
        assert_eq!(choose_site(None, None, None), SiteChoice::NotYet);
        assert_eq!(
            choose_site(None, Some(&repo), Some(&readme)),
            SiteChoice::NotYet
        );
    }

    #[test]
    fn a_relative_argument_comes_back_absolute() {
        let SiteChoice::Chosen(site) = choose_site(Some(Path::new("some-site")), None, None) else {
            panic!("an argument is always a choice");
        };
        assert!(site.is_absolute(), "{}", site.display());
        assert_eq!(
            site,
            std::env::current_dir().unwrap().join("some-site"),
            "resolved against the working directory"
        );
    }

    #[test]
    fn a_file_of_the_served_site_belongs() {
        let tree = Tree::new();
        let a = tree.site("a");
        let file = tree.file("a/content/post/x.md");
        assert_eq!(file_in_session(&a, &file), FilePlace::Belongs);
    }

    #[test]
    fn a_file_of_another_site_names_it() {
        let tree = Tree::new();
        let a = tree.site("a");
        let b = tree.site("b");
        let file = tree.file("b/content/post/x.md");
        assert_eq!(file_in_session(&a, &file), FilePlace::OtherSite(b));
    }

    #[test]
    fn a_file_in_no_site_is_no_site() {
        let tree = Tree::new();
        let a = tree.site("a");
        let readme = tree.file("README.md");
        assert_eq!(file_in_session(&a, &readme), FilePlace::NoSite);
    }

    #[test]
    fn a_file_of_another_site_is_told_how_to_get_its_own_server() {
        let diagnostic =
            other_site_diagnostic(Path::new("/work/site-b"), Path::new("/work/site-a"));
        assert_eq!(diagnostic.severity, Some(DiagnosticSeverity::INFORMATION));
        assert_eq!(diagnostic.range.start, Position::new(0, 0));
        assert_eq!(
            diagnostic.message,
            "This file belongs to site `/work/site-b`, but this language server serves \
             `/work/site-a`. Save it, then close and reopen it, to give it a language \
             server for its own site."
        );
    }
}
