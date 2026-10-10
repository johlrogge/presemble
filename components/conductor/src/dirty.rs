use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Why a document has unsaved edits in the conductor's store.
///
/// A conflicted document is dirty by construction: it is the same unsaved
/// edits, plus the fact that the file on disk changed under them (ADR-065).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsaved {
    /// Edited here; the file on disk is as we last saw it.
    Edited,
    /// Edited here, and the file on disk changed since. `disk_mtime` is the
    /// modification time of the disk version that conflicts.
    ConflictsWithDisk { disk_mtime: SystemTime },
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

    /// Whether the file at `rel` differs from what we last saw. A file that
    /// was there and is now gone counts as changed.
    pub fn changed(&self, rel: &Path, current: Option<DiskStamp>) -> bool {
        match self.stamps.get(rel) {
            Some(recorded) => current != Some(*recorded),
            None => false,
        }
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
    /// construction). Returns whether the conflict is new: it was not already
    /// recorded, so the caller should report it once.
    pub fn mark_conflict(&mut self, path: impl Into<PathBuf>, disk_mtime: SystemTime) -> bool {
        let previous = self
            .docs
            .insert(path.into(), Unsaved::ConflictsWithDisk { disk_mtime });
        !matches!(previous, Some(Unsaved::ConflictsWithDisk { .. }))
    }

    /// Remove a document from the dirty set (after save or reload), clearing
    /// any conflict with it.
    pub fn clear(&mut self, path: &Path) {
        self.docs.remove(path);
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
        dirty.clear(Path::new("content/post/hello.md"));
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
        assert!(dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH));
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
        assert!(dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH));
        assert!(
            !dirty.mark_conflict("content/a.md", SystemTime::now()),
            "already recorded"
        );
    }

    #[test]
    fn more_edits_do_not_resolve_a_conflict() {
        let mut dirty = DirtyDocs::new();
        dirty.mark("content/a.md");
        assert!(!dirty.is_conflicted(Path::new("content/a.md")));
        dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH);
        dirty.mark("content/a.md");
        assert!(dirty.is_conflicted(Path::new("content/a.md")));
    }

    #[test]
    fn clear_clears_a_conflict() {
        let mut dirty = DirtyDocs::new();
        dirty.mark_conflict("content/a.md", SystemTime::UNIX_EPOCH);
        dirty.clear(Path::new("content/a.md"));
        assert!(!dirty.contains(Path::new("content/a.md")));
        assert!(!dirty.is_conflicted(Path::new("content/a.md")));
        assert_eq!(dirty.conflicts().count(), 0);
    }
}
