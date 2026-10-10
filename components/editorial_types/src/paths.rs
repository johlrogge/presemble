//! Client-supplied file paths, validated at the boundary.
//!
//! A path that reaches the conductor from a client (LSP, MCP, browser, CLI) is
//! a [`SitePath`]: relative, made only of plain names, never decoded. A
//! [`ContentPath`] is a `SitePath` that also names a markdown file under
//! `content/`. Deserialisation validates, so an invalid path is refused when
//! the message is decoded and never reaches a handler.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// Why a string is not an acceptable site path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    Absolute,
    /// A component that is not a plain name (`..`, `.`).
    NotNormal {
        component: String,
    },
    Nul,
    /// A content path must be under `content/`.
    NotContent,
    /// A content path must end in `.md`.
    NotMarkdown,
    /// The path leaves the site directory (for example through a symlink).
    OutsideSite,
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Empty => f.write_str("path is empty"),
            PathError::Absolute => f.write_str("path must be relative to the site"),
            PathError::NotNormal { component } => {
                write!(f, "path component is not a plain name: {component:?}")
            }
            PathError::Nul => f.write_str("path contains a NUL byte"),
            PathError::NotContent => f.write_str("path must be under 'content/'"),
            PathError::NotMarkdown => f.write_str("path must end with '.md'"),
            PathError::OutsideSite => f.write_str("path leaves the site directory"),
        }
    }
}

impl std::error::Error for PathError {}

/// `path` with symlinks resolved as far as it exists, followed by the part that
/// does not exist yet. A symlink that cannot be resolved (dangling, a loop) is
/// refused: writing through it would create its target.
pub fn canonical_deepest(path: &Path) -> Result<PathBuf, PathError> {
    let mut existing = path;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if existing.symlink_metadata().is_ok() {
            let mut resolved = existing
                .canonicalize()
                .map_err(|_| PathError::OutsideSite)?;
            for name in tail.iter().rev() {
                resolved.push(name);
            }
            return Ok(resolved);
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                existing = parent;
            }
            _ => return Err(PathError::OutsideSite),
        }
    }
}

/// The names of a URL path (`/post/hello`, `/post/`), each a plain name.
///
/// Empty segments (a leading, trailing or doubled `/`) are skipped. `.` and
/// `..`, a NUL byte and a backslash are refused: a URL segment becomes a
/// directory or file name, and none of those is one. Nothing is
/// percent-decoded here or anywhere after.
pub fn plain_url_segments(url: &str) -> Result<Vec<&str>, PathError> {
    let mut names = Vec::new();
    for segment in url.split('/').filter(|s| !s.is_empty()) {
        if segment == "." || segment == ".." {
            return Err(PathError::NotNormal {
                component: segment.to_string(),
            });
        }
        if segment.contains('\0') {
            return Err(PathError::Nul);
        }
        if segment.contains('\\') {
            return Err(PathError::NotNormal {
                component: segment.to_string(),
            });
        }
        names.push(segment);
    }
    Ok(names)
}

/// `path` lies inside `base` once symlinks are resolved (as far as it exists).
pub fn ensure_within(base: &Path, path: &Path) -> Result<(), PathError> {
    let base = canonical_deepest(base)?;
    let target = canonical_deepest(path)?;
    if target.starts_with(&base) {
        Ok(())
    } else {
        Err(PathError::OutsideSite)
    }
}

/// A path inside the site directory, written relative to it.
///
/// Never URL-decoded: `%2e%2e` is a file name like any other.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SitePath(String);

impl SitePath {
    pub fn parse(s: &str) -> Result<SitePath, PathError> {
        if s.is_empty() {
            return Err(PathError::Empty);
        }
        if s.contains('\0') {
            return Err(PathError::Nul);
        }
        let path = Path::new(s);
        if path.has_root()
            || path
                .components()
                .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        {
            return Err(PathError::Absolute);
        }
        for component in path.components() {
            match component {
                Component::Normal(_) => {}
                other => {
                    return Err(PathError::NotNormal {
                        component: other.as_os_str().to_string_lossy().into_owned(),
                    });
                }
            }
        }
        // `Path::components` hides `.` inside a path ("a/./b") and trailing
        // slashes; what is stored is the normalised form, so reject any
        // spelling that differs from it.
        let normalised: PathBuf = path.components().collect();
        if normalised.to_string_lossy() != s {
            return Err(PathError::NotNormal {
                component: s.to_string(),
            });
        }
        Ok(SitePath(s.to_string()))
    }

    /// `abs` relative to `site_dir`. A path that is not under `site_dir` is
    /// `OutsideSite`.
    ///
    /// When the words do not line up (either side is spelled through a
    /// symlink, e.g. `/tmp` vs `/private/tmp`), both are resolved as far as
    /// they exist ([`canonical_deepest`]) and compared again.
    pub fn from_absolute(site_dir: &Path, abs: &Path) -> Result<SitePath, PathError> {
        if let Ok(rel) = abs.strip_prefix(site_dir) {
            return SitePath::parse(&rel.to_string_lossy());
        }
        let site = canonical_deepest(site_dir)?;
        let target = canonical_deepest(abs)?;
        let rel = target
            .strip_prefix(&site)
            .map_err(|_| PathError::OutsideSite)?;
        SitePath::parse(&rel.to_string_lossy())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl fmt::Display for SitePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for SitePath {
    type Error = PathError;
    fn try_from(s: String) -> Result<Self, PathError> {
        SitePath::parse(&s)
    }
}

impl From<SitePath> for String {
    fn from(p: SitePath) -> String {
        p.0
    }
}

/// A markdown file under `content/` (e.g. "content/post/hello.md").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentPath(SitePath);

impl ContentPath {
    pub fn parse(s: &str) -> Result<ContentPath, PathError> {
        let site = SitePath::parse(s)?;
        if !site.as_path().starts_with("content") || site.as_str() == "content" {
            return Err(PathError::NotContent);
        }
        if !s.ends_with(".md") {
            return Err(PathError::NotMarkdown);
        }
        Ok(ContentPath(site))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_site_path(&self) -> &SitePath {
        &self.0
    }

    /// Resolve to an absolute path given a site directory. This joins only;
    /// the conductor's resolver also checks symlinks.
    pub fn resolve(&self, site_dir: &Path) -> PathBuf {
        site_dir.join(self.0.as_str())
    }
}

impl fmt::Display for ContentPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl TryFrom<String> for ContentPath {
    type Error = PathError;
    fn try_from(s: String) -> Result<Self, PathError> {
        ContentPath::parse(&s)
    }
}

impl From<ContentPath> for String {
    fn from(p: ContentPath) -> String {
        p.0.into()
    }
}

impl From<ContentPath> for SitePath {
    fn from(p: ContentPath) -> SitePath {
        p.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_path_refuses_parent_segments() {
        for bad in [
            "../outside.md",
            "content/../../x.md",
            "content/a/../../../x",
        ] {
            assert!(
                matches!(SitePath::parse(bad), Err(PathError::NotNormal { .. })),
                "{bad} must be refused, got {:?}",
                SitePath::parse(bad)
            );
        }
    }

    #[test]
    fn site_path_refuses_absolute_paths() {
        for bad in ["/etc/passwd", "/tmp/x.md"] {
            assert_eq!(SitePath::parse(bad), Err(PathError::Absolute), "{bad}");
        }
    }

    #[test]
    fn site_path_refuses_dot_segments() {
        for bad in ["./content/a.md", "content/./a.md"] {
            assert!(
                matches!(SitePath::parse(bad), Err(PathError::NotNormal { .. })),
                "{bad} must be refused, got {:?}",
                SitePath::parse(bad)
            );
        }
    }

    #[test]
    fn site_path_refuses_nul_and_empty() {
        assert_eq!(SitePath::parse(""), Err(PathError::Empty));
        assert_eq!(SitePath::parse("content/a\0.md"), Err(PathError::Nul));
    }

    /// GUARD: passes on the stub too, because nothing ever decodes a path.
    /// It pins that `%2e%2e` stays a literal name.
    #[test]
    fn percent_encoded_dots_are_literal_names() {
        let p = SitePath::parse("content/%2e%2e/x.md").expect("a plain name");
        assert_eq!(p.as_str(), "content/%2e%2e/x.md");
    }

    #[test]
    fn site_path_accepts_plain_relative_paths() {
        let p = SitePath::parse("content/post/hello.md").expect("valid");
        assert_eq!(p.as_str(), "content/post/hello.md");
    }

    #[test]
    fn site_path_from_absolute_strips_the_site_dir_and_refuses_outsiders() {
        let site = Path::new("/srv/site");
        let inside =
            SitePath::from_absolute(site, Path::new("/srv/site/content/a.md")).expect("inside");
        assert_eq!(inside.as_str(), "content/a.md");
        assert_eq!(
            SitePath::from_absolute(site, Path::new("/srv/other/a.md")),
            Err(PathError::OutsideSite)
        );
        assert!(SitePath::from_absolute(site, Path::new("/srv/site/../other/a.md")).is_err());
    }

    /// `<tmp>/real/site`, `<tmp>/link -> real`, `<tmp>/real/outside`.
    #[cfg(unix)]
    fn symlinked_world() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("real/site/content")).unwrap();
        std::fs::create_dir_all(tmp.path().join("real/outside")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("link")).unwrap();
        tmp
    }

    #[cfg(unix)]
    #[test]
    fn from_absolute_accepts_a_path_through_a_symlinked_site_dir() {
        let tmp = symlinked_world();
        let site = tmp.path().canonicalize().unwrap().join("real/site");
        let through_link = tmp.path().join("link/site/content/a.md");
        let got = SitePath::from_absolute(&site, &through_link).expect("inside the site");
        assert_eq!(got.as_str(), "content/a.md");
        // A file that does not exist yet is accepted the same way.
        assert!(
            SitePath::from_absolute(&site, &tmp.path().join("link/site/content/new/b.md")).is_ok()
        );
    }

    /// GUARD: a path that reaches outside the site through a symlink stays
    /// refused (spelled through the link, or by the real location).
    #[cfg(unix)]
    #[test]
    fn from_absolute_still_refuses_a_path_outside_through_a_symlink() {
        let tmp = symlinked_world();
        let site = tmp.path().canonicalize().unwrap().join("real/site");
        for outside in ["link/outside/x.md", "real/outside/x.md", "link/other.md"] {
            assert_eq!(
                SitePath::from_absolute(&site, &tmp.path().join(outside)),
                Err(PathError::OutsideSite),
                "{outside}"
            );
        }
    }

    #[test]
    fn plain_url_segments_keep_names_and_refuse_the_rest() {
        assert_eq!(plain_url_segments("/post/hello"), Ok(vec!["post", "hello"]));
        assert_eq!(plain_url_segments("/post/"), Ok(vec!["post"]));
        assert_eq!(plain_url_segments("/"), Ok(vec![]));
        for bad in ["/../../outside", "/post/./x", "/post/..", "/a\\b", "/a\0b"] {
            assert!(plain_url_segments(bad).is_err(), "{bad:?} must be refused");
        }
        // Nothing is decoded: this is a plain (odd) name.
        assert_eq!(plain_url_segments("/%2e%2e/x"), Ok(vec!["%2e%2e", "x"]));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_within_refuses_a_symlink_out_of_the_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("out");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(tmp.path().join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere"), base.join("escape")).unwrap();
        assert!(ensure_within(&base, &base.join("post/x/index.html")).is_ok());
        assert_eq!(
            ensure_within(&base, &base.join("escape/index.html")),
            Err(PathError::OutsideSite)
        );
        assert_eq!(
            ensure_within(&base, &base.join("../elsewhere/index.html")),
            Err(PathError::OutsideSite)
        );
    }

    #[test]
    fn content_path_requires_content_dir_and_md() {
        assert!(ContentPath::parse("content/post/hello.md").is_ok());
        assert_eq!(
            ContentPath::parse("schemas/bad.md"),
            Err(PathError::NotContent)
        );
        assert_eq!(
            ContentPath::parse("content/post/bad.txt"),
            Err(PathError::NotMarkdown)
        );
        // Ported from content_editor::validate_content_path (`..` rule).
        assert!(matches!(
            ContentPath::parse("content/../etc/passwd.md"),
            Err(PathError::NotNormal { .. })
        ));
        assert!(ContentPath::parse("/content/post/a.md").is_err());
        assert_eq!(
            ContentPath::parse("contents/a.md"),
            Err(PathError::NotContent)
        );
    }

    #[test]
    fn content_path_deserialization_validates() {
        let bad: Result<ContentPath, _> = serde_json::from_str("\"../x.md\"");
        assert!(bad.is_err(), "a traversal path must fail to decode");
        let bad_site: Result<SitePath, _> = serde_json::from_str("\"/etc/passwd\"");
        assert!(bad_site.is_err(), "an absolute path must fail to decode");
        let good: ContentPath = serde_json::from_str("\"content/a.md\"").expect("valid");
        assert_eq!(good.as_str(), "content/a.md");
        assert_eq!(
            serde_json::to_string(&good).expect("serialize"),
            "\"content/a.md\""
        );
    }
}
