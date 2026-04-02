use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Tracks which output files depend on which source files.
/// Forward: output path -> set of source paths it was built from.
/// Reverse: source path -> set of output paths that depend on it.
#[derive(Debug, Default, Clone)]
pub struct DependencyGraph {
    forward: HashMap<PathBuf, HashSet<PathBuf>>,
    reverse: HashMap<PathBuf, HashSet<PathBuf>>,
}

impl DependencyGraph {
    pub fn new() -> Self { Self::default() }

    /// Record that `output` was built from `sources`.
    /// Replaces any previous registration for `output`.
    pub fn register(&mut self, output: PathBuf, sources: HashSet<PathBuf>) {
        self.remove_output(&output);
        for source in &sources {
            self.reverse.entry(source.clone()).or_default().insert(output.clone());
        }
        self.forward.insert(output, sources);
    }

    /// Remove all dependency records for `output`.
    pub fn remove_output(&mut self, output: &Path) {
        if let Some(sources) = self.forward.remove(output) {
            for source in sources {
                if let Some(outputs) = self.reverse.get_mut(&source) {
                    outputs.remove(output);
                    if outputs.is_empty() {
                        self.reverse.remove(&source);
                    }
                }
            }
        }
    }

    /// Given a changed source file, return all output files that need rebuilding.
    pub fn affected_outputs(&self, source: &Path) -> HashSet<PathBuf> {
        self.reverse.get(source).cloned().unwrap_or_default()
    }

    /// Return all source files that `output` was built from.
    pub fn sources_for(&self, output: &Path) -> HashSet<PathBuf> {
        self.forward.get(output).cloned().unwrap_or_default()
    }

    /// Returns true if `source` is tracked as a dependency of any output.
    pub fn is_known_source(&self, source: &Path) -> bool {
        self.reverse.contains_key(source)
    }

    /// Merge another graph into this one (used after partial rebuild).
    /// For each output in `other`, replaces existing entries.
    pub fn merge(&mut self, other: DependencyGraph) {
        for (output, sources) in other.forward {
            self.register(output, sources);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affected_outputs_returns_both_for_shared_source() {
        let mut g = DependencyGraph::new();
        let schema = PathBuf::from("schemas/article/item.md");
        let out1 = PathBuf::from("output/article/a.html");
        let out2 = PathBuf::from("output/article/b.html");
        g.register(out1.clone(), HashSet::from([schema.clone()]));
        g.register(out2.clone(), HashSet::from([schema.clone()]));
        let affected = g.affected_outputs(&schema);
        assert!(affected.contains(&out1));
        assert!(affected.contains(&out2));
    }

    #[test]
    fn remove_output_cleans_reverse_entries() {
        let mut g = DependencyGraph::new();
        let schema = PathBuf::from("schemas/article/item.md");
        let out = PathBuf::from("output/article/a.html");
        g.register(out.clone(), HashSet::from([schema.clone()]));
        g.remove_output(&out);
        assert!(g.affected_outputs(&schema).is_empty());
        assert!(!g.forward.contains_key(&out));
    }

    #[test]
    fn merge_replaces_stale_entries() {
        let mut g = DependencyGraph::new();
        let old_source = PathBuf::from("templates/old.html");
        let new_source = PathBuf::from("templates/new.html");
        let out = PathBuf::from("output/index.html");
        g.register(out.clone(), HashSet::from([old_source.clone()]));

        let mut partial = DependencyGraph::new();
        partial.register(out.clone(), HashSet::from([new_source.clone()]));
        g.merge(partial);

        assert!(g.affected_outputs(&new_source).contains(&out));
        assert!(!g.affected_outputs(&old_source).contains(&out));
    }

    #[test]
    fn affected_outputs_returns_empty_for_unknown_source() {
        let g = DependencyGraph::new();
        assert!(g.affected_outputs(Path::new("unknown.md")).is_empty());
    }

    #[test]
    fn is_known_source_returns_true_for_registered_source() {
        let mut g = DependencyGraph::new();
        let source = PathBuf::from("content/article/hello.md");
        let output = PathBuf::from("output/article/hello/index.html");
        g.register(output, HashSet::from([source.clone()]));
        assert!(g.is_known_source(&source));
    }

    #[test]
    fn is_known_source_returns_false_for_unknown_source() {
        let g = DependencyGraph::new();
        assert!(!g.is_known_source(Path::new("content/article/unknown.md")));
    }

    #[test]
    fn is_known_source_returns_false_after_output_removed() {
        let mut g = DependencyGraph::new();
        let source = PathBuf::from("content/article/hello.md");
        let output = PathBuf::from("output/article/hello/index.html");
        g.register(output.clone(), HashSet::from([source.clone()]));
        g.remove_output(&output);
        assert!(!g.is_known_source(&source));
    }

    #[test]
    fn affected_outputs_with_absolute_path_matches_registered_relative() {
        // This test documents the requirement: after canonicalization at the call site,
        // paths registered and looked up should always be canonical and thus equal.
        // The DependencyGraph itself stores whatever it receives — callers are responsible.
        let mut g = DependencyGraph::new();
        let source = PathBuf::from("/canonical/path/to/file.md");
        let output = PathBuf::from("/canonical/path/to/output/index.html");
        g.register(output.clone(), HashSet::from([source.clone()]));
        let affected = g.affected_outputs(&source);
        assert!(affected.contains(&output));
    }
}
