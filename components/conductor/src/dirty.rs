use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::protocol::ConflictRegion;

/// Why a document has unsaved edits in the conductor's store.
///
/// A conflicted document is dirty by construction: it is the same unsaved
/// edits, plus the fact that the file on disk changed under them (ADR-065).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsaved {
    /// Edited here; the file on disk is as we last saw it.
    Edited,
    /// Edited here, and the file on disk changed since. `disk_mtime` is the
    /// modification time of the disk version that conflicts; `regions` are the
    /// blocks that changed differently on both sides (ADR-065 amendment F2).
    /// No regions: the whole file conflicts.
    ConflictsWithDisk {
        disk_mtime: SystemTime,
        regions: Vec<ConflictRegion>,
    },
}

/// What a file on disk looked like when the conductor last read or wrote it:
/// its modification time and its length. The length catches a change that lands
/// in the same mtime tick as the one we recorded (coarse timestamps).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskStamp {
    mtime: SystemTime,
    len: u64,
}

impl DiskStamp {
    /// The stamp of the file at `path` now; `None` when it does not exist (or
    /// cannot be read).
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self {
            mtime: meta.modified().ok()?,
            len: meta.len(),
        })
    }
}

/// The [`DiskStamp`] of each content file (relative to site root) at the
/// moment the conductor's copy of it was read from disk or written to it.
///
/// A save compares the file's current stamp with this one: a difference means
/// something else wrote the file since, whether or not the watcher has said so
/// yet (it debounces for 150 ms). A path with no entry has no baseline and is
/// never reported as changed.
#[derive(Debug, Default)]
pub struct DiskStamps {
    stamps: HashMap<PathBuf, DiskStamp>,
}

impl DiskStamps {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember the stamp of the file as we last saw it. `None` (the file is
    /// missing) forgets the path: there is nothing to compare against.
    pub fn record(&mut self, rel: impl Into<PathBuf>, stamp: Option<DiskStamp>) {
        let rel = rel.into();
        match stamp {
            Some(stamp) => {
                self.stamps.insert(rel, stamp);
            }
            None => {
                self.stamps.remove(&rel);
            }
        }
    }

    /// Whether `stamp` is exactly the one recorded for `rel`: the file is the
    /// version our copy was read from or last written as. A path with no
    /// recorded stamp, or a missing file, matches nothing.
    pub fn matches(&self, rel: &Path, stamp: Option<DiskStamp>) -> bool {
        stamp.is_some() && self.stamps.get(rel).copied() == stamp
    }

    /// Whether the file at `rel` differs from what we last saw. A file that
    /// was there and is now gone counts as changed.
    pub fn changed(&self, rel: &Path, current: Option<DiskStamp>) -> bool {
        match self.stamps.get(rel) {
            Some(recorded) => current != Some(*recorded),
            None => false,
        }
    }
}

/// The base of each document's unsaved edits (ADR-065 amendment F2): the
/// text of its file (relative to site root) as N last agreed with it, read
/// when the document went from saved to unsaved. A later disk change is
/// merged against it block by block. A dirty document without a base (its
/// file had already changed under N) can only conflict as a whole.
#[derive(Debug, Default)]
pub struct DiskBases {
    bases: HashMap<PathBuf, String>,
}

impl DiskBases {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember `text` as the base of the document at `rel`.
    pub fn record(&mut self, rel: impl Into<PathBuf>, text: String) {
        self.bases.insert(rel.into(), text);
    }

    /// The base of the document at `rel`, if it has one.
    pub(crate) fn get(&self, rel: &Path) -> Option<&str> {
        self.bases.get(rel).map(String::as_str)
    }

    /// Forget the base of the document at `rel` (it was saved, reloaded or
    /// settled: it has no unsaved edits any more).
    pub fn clear(&mut self, rel: &Path) {
        self.bases.remove(rel);
    }
}

/// Content file paths (relative to site root, e.g. `"content/post/hello.md"`)
/// whose NodeStore subtree has been mutated since the last save, each with its
/// [`Unsaved`] state.
///
/// An editor's typing is in the store too (it is lowered into N as it is
/// typed), so a document an editor changes is dirty here like any other.
/// `DirtyDocs` uses the same content-relative path convention as the
/// NodeStore's `:file` attribute.
#[derive(Debug, Default)]
pub struct DirtyDocs {
    docs: HashMap<PathBuf, Unsaved>,
}

impl DirtyDocs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a document as dirty. A document already in conflict stays in
    /// conflict: more edits do not resolve it.
    pub fn mark(&mut self, path: impl Into<PathBuf>) {
        self.docs.entry(path.into()).or_insert(Unsaved::Edited);
    }

    /// Mark a document as in conflict with the file on disk (it is dirty by
    /// construction), on `regions` (none: the whole file); the regions replace
    /// any recorded before. Returns whether the caller should announce the
    /// conflict: the document was not in conflict, or the set of its region
    /// ids changed (a new region, one gone, or one whose disk text changed).
    /// The identical set seen again is not announced.
    pub fn mark_conflict(
        &mut self,
        path: impl Into<PathBuf>,
        disk_mtime: SystemTime,
        regions: Vec<ConflictRegion>,
    ) -> bool {
        let path = path.into();
        let ids = |regions: &[ConflictRegion]| {
            regions
                .iter()
                .map(|r| r.id)
                .collect::<std::collections::HashSet<_>>()
        };
        let new = match self.docs.get(&path) {
            Some(Unsaved::ConflictsWithDisk {
                regions: previous, ..
            }) => ids(previous) != ids(&regions),
            _ => true,
        };
        self.docs.insert(
            path,
            Unsaved::ConflictsWithDisk {
                disk_mtime,
                regions,
            },
        );
        new
    }

    /// The document's last conflict is resolved: it is an ordinary unsaved
    /// document again. Returns whether it was in conflict (the end to
    /// announce).
    #[must_use]
    pub fn clear_conflict(&mut self, path: &Path) -> bool {
        match self.docs.get_mut(path) {
            Some(state @ Unsaved::ConflictsWithDisk { .. }) => {
                *state = Unsaved::Edited;
                true
            }
            _ => false,
        }
    }

    /// The regions a document conflicts on (`Some(&[])`: the whole file);
    /// `None` when it is not in conflict.
    pub fn conflict_regions(&self, path: &Path) -> Option<&[ConflictRegion]> {
        match self.docs.get(path) {
            Some(Unsaved::ConflictsWithDisk { regions, .. }) => Some(regions),
            _ => None,
        }
    }

    /// Remove a document from the dirty set (after save or reload), clearing
    /// any conflict with it. Returns whether it was in conflict (the end to
    /// announce).
    #[must_use]
    pub fn clear(&mut self, path: &Path) -> bool {
        matches!(
            self.docs.remove(path),
            Some(Unsaved::ConflictsWithDisk { .. })
        )
    }

    /// Check whether a document is dirty (edited or in conflict).
    pub fn contains(&self, path: &Path) -> bool {
        self.docs.contains_key(path)
    }

    /// Check whether a document is in conflict with the file on disk.
    pub fn is_conflicted(&self, path: &Path) -> bool {
        matches!(self.docs.get(path), Some(Unsaved::ConflictsWithDisk { .. }))
    }

    /// The documents in conflict with the file on disk.
    pub fn conflicts(&self) -> impl Iterator<Item = &Path> {
        self.docs
            .iter()
            .filter(|(_, state)| matches!(state, Unsaved::ConflictsWithDisk { .. }))
            .map(|(path, _)| path.as_path())
    }

    /// True if no documents are dirty.
    pub fn is_empty(&self) -> bool {
        self.docs.is_empty()
    }

    /// Iterate over dirty paths (borrowing — doesn't modify the set).
    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        self.docs.keys().map(|p| p.as_path())
    }

    /// Take (drain) all dirty documents with their state, leaving the set
    /// empty. Used for SaveAllBuffers.
    pub fn take(&mut self) -> HashMap<PathBuf, Unsaved> {
        std::mem::take(&mut self.docs)
    }

    /// Put a document back with the state it had (a save of it failed).
    pub fn restore(&mut self, path: PathBuf, state: Unsaved) {
        self.docs.insert(path, state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        assert!(DirtyDocs::new().is_empty());
    }

    #[test]
    fn mark_then_contains() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/post/hello.md");
        assert!(dirty.contains(Path::new("content/post/hello.md")));
    }

    #[test]
    fn mark_then_clear_then_not_contains() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/post/hello.md");
        assert!(
            !dirty.clear(Path::new("content/post/hello.md")),
            "it was not in conflict"
        );
        assert!(!dirty.contains(Path::new("content/post/hello.md")));
    }

    #[test]
    fn mark_same_path_twice_stays_dedup() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/post/hello.md");
        dirty.mark("content/post/hello.md");
        let count = dirty.iter().count();
        assert_eq!(count, 1, "same path marked twice should appear only once");
    }

    #[test]
    fn take_drains() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/post/a.md");
        dirty.mark("content/post/b.md");
        let taken = dirty.take();
        assert_eq!(taken.len(), 2, "take should return both paths");
        assert!(dirty.is_empty(), "set should be empty after take");
    }

    #[test]
    fn iter_does_not_mutate() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/post/x.md");
        dirty.mark("content/post/y.md");
        let first_count = dirty.iter().count();
        let second_count = dirty.iter().count();
        assert_eq!(first_count, 2);
        assert_eq!(
            second_count, 2,
            "second iteration should still yield both paths"
        );
    }

    #[test]
    fn a_conflicted_document_is_dirty() {
        let mut dirty = DirtyDocs::new();
        assert!(dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH, vec![]));
        assert!(dirty.contains(Path::new("content/a.md")));
        assert!(dirty.is_conflicted(Path::new("content/a.md")));
        assert_eq!(
            dirty.conflicts().collect::<Vec<_>>(),
            vec![Path::new("content/a.md")]
        );
        assert_eq!(dirty.iter().count(), 1);
    }

    #[test]
    fn a_conflict_is_reported_once() {
        let mut dirty = DirtyDocs::new();
        assert!(dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH, vec![]));
        assert!(
            !dirty.mark_conflict("content/a.md", SystemTime::now(), vec![]),
            "already recorded"
        );
    }

    #[test]
    fn more_edits_do_not_resolve_a_conflict() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/a.md");
        assert!(!dirty.is_conflicted(Path::new("content/a.md")));
        dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH, vec![]);
        dirty.mark("content/a.md");
        assert!(dirty.is_conflicted(Path::new("content/a.md")));
    }

    #[test]
    fn a_base_is_recorded_and_cleared() {
        let mut bases = DiskBases::new();
        assert_eq!(bases.get(Path::new("content/a.md")), None);
        bases.record("content/a.md", "# A\n".to_string());
        assert_eq!(bases.get(Path::new("content/a.md")), Some("# A\n"));
        bases.clear(Path::new("content/a.md"));
        assert_eq!(bases.get(Path::new("content/a.md")), None);
    }

    #[test]
    fn clear_clears_a_conflict() {
        let mut dirty = DirtyDocs::new();
        dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH, vec![]);
        assert!(dirty.clear(Path::new("content/a.md")), "it was in conflict");
        assert!(!dirty.contains(Path::new("content/a.md")));
        assert!(!dirty.is_conflicted(Path::new("content/a.md")));
        assert_eq!(dirty.conflicts().count(), 0);
    }

    #[test]
    fn clear_conflict_says_whether_there_was_one() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/a.md");
        assert!(!dirty.clear_conflict(Path::new("content/a.md")));
        dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH, vec![]);
        assert!(dirty.clear_conflict(Path::new("content/a.md")));
        assert!(dirty.contains(Path::new("content/a.md")), "still unsaved");
        assert!(!dirty.is_conflicted(Path::new("content/a.md")));
    }
}
