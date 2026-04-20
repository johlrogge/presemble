use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Set of content file paths (relative to site root, e.g. `"content/post/hello.md"`)
/// whose NodeStore subtree has been mutated since the last save.
///
/// Separate from `doc_sources`, which is an LSP working-copy buffer keyed on
/// absolute paths. `DirtyDocs` uses the same content-relative path convention
/// as the NodeStore's `:file` attribute.
#[derive(Debug, Default)]
pub struct DirtyDocs {
    paths: HashSet<PathBuf>,
}

impl DirtyDocs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a document as dirty.
    pub fn mark(&mut self, path: impl Into<PathBuf>) {
        self.paths.insert(path.into());
    }

    /// Remove a document from the dirty set (after successful save).
    pub fn clear(&mut self, path: &Path) {
        self.paths.remove(path);
    }

    /// Check whether a document is dirty.
    pub fn contains(&self, path: &Path) -> bool {
        self.paths.contains(path)
    }

    /// True if no documents are dirty.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Iterate over dirty paths (borrowing — doesn't modify the set).
    pub fn iter(&self) -> impl Iterator<Item = &Path> {
        self.paths.iter().map(|p| p.as_path())
    }

    /// Take (drain) all dirty paths, leaving the set empty.
    /// Used for SaveAllBuffers.
    pub fn take(&mut self) -> HashSet<PathBuf> {
        std::mem::take(&mut self.paths)
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
        assert_eq!(second_count, 2, "second iteration should still yield both paths");
    }
}
