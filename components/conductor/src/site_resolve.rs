//! The one way a client path becomes a file path.
//!
//! A [`SitePath`] is already lexically safe (relative, plain names only). What
//! it cannot know is the file system: a symlink inside the site may point out
//! of it. [`resolve_in_site`] joins, resolves symlinks on the deepest part that
//! exists, and refuses a result outside the site directory.

use editorial_types::{canonical_deepest, PathError, SitePath};
use std::path::{Path, PathBuf};

/// `path` as a file path under `site_dir`, or `OutsideSite` when it resolves
/// (through a symlink) to somewhere else.
///
/// The returned path is the plain join, not the canonical one, so it stays
/// comparable with the paths the conductor keys its own bookkeeping by.
pub(crate) fn resolve_in_site(site_dir: &Path, path: &SitePath) -> Result<PathBuf, PathError> {
    let joined = site_dir.join(path.as_path());
    let site = canonical_deepest(site_dir)?;
    let target = canonical_deepest(&joined)?;
    if target.starts_with(&site) { Ok(joined) } else { Err(PathError::OutsideSite) }
}
