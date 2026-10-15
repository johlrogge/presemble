//! Test sites live one level inside their temp dir.
//!
//! A copy of `components/conductor/src/test_site.rs`, the original.
//!
//! A site's output goes beside it: `<parent>/output/<name>` and
//! `<parent>/output-editorial/<name>`. A site at a temp dir's root therefore
//! writes into the shared temp dir (`/tmp/output/.tmpXXXX`), where nothing
//! removes it. A site at `<tmp>/site` writes `<tmp>/output…/site`, which goes
//! away with the temp dir.

use std::path::{Path, PathBuf};

/// Create and return the site directory inside `tmp`: `tmp/site`.
pub(crate) fn site_in(tmp: &tempfile::TempDir) -> PathBuf {
    let site = tmp.path().join("site");
    std::fs::create_dir_all(&site).expect("create the site dir");
    site
}

/// A temp dir and the site inside it; dropping it removes both, and the
/// output the site's builds wrote beside it. `path()` is the site, as
/// `TempDir::path()` was when the site was the temp dir itself.
pub(crate) struct TestSite {
    tmp: tempfile::TempDir,
    site: PathBuf,
}

impl TestSite {
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let site = site_in(&tmp);
        TestSite { tmp, site }
    }

    /// The site directory.
    pub(crate) fn path(&self) -> &Path {
        &self.site
    }

    /// The temp dir that holds the site (and its output).
    pub(crate) fn temp_dir(&self) -> &Path {
        self.tmp.path()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_site_writes_nothing_beside_its_temp_dir() {
        let site = TestSite::new();
        let root = site.path();
        std::fs::create_dir_all(root.join("schemas/post")).unwrap();
        std::fs::create_dir_all(root.join("templates/post")).unwrap();
        std::fs::create_dir_all(root.join("content/post")).unwrap();
        std::fs::write(
            root.join("schemas/post/item.md"),
            "# Title {#title}\noccurs\n: exactly once\n",
        )
        .unwrap();
        std::fs::write(
            root.join("templates/post/item.hiccup"),
            "[:main [:presemble/insert {:data input.title :as \"h1\"}]]",
        )
        .unwrap();
        std::fs::write(root.join("content/post/hello.md"), "# Hello\n").unwrap();

        let _conductor = crate::open_conductor(root).expect("conductor");

        let tmp = site.temp_dir();
        let name = tmp.file_name().expect("a temp dir name");
        let parent = tmp.parent().expect("a temp dir parent");
        assert!(
            !parent.join("output-editorial").join(name).exists(),
            "nothing is written beside the temp dir"
        );
        assert!(
            tmp.join("output-editorial/site").exists(),
            "the site's output is inside the temp dir"
        );
    }
}
