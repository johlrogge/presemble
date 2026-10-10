//! A guard for ADR-059 stage 5b: a deprecation is never silenced for a whole
//! crate. The `#![allow(deprecated)]` attributes that covered the `Link` and
//! `Image` block kinds were removed with the kinds in 5b.2; a crate-level one
//! must not come back. (An `#[allow(deprecated)]` on one item is a different
//! thing and is not checked here.)

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_some_and(|n| n == "target" || n == ".git")
            {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_crate_level_allow_deprecated() {
    let root = repo_root();
    let mut files = Vec::new();
    for top in ["components", "bases", "projects"] {
        rust_sources(&root.join(top), &mut files);
    }
    assert!(
        files.len() > 100,
        "found only {} source files; is the root right?",
        files.len()
    );
    let offenders: Vec<String> = files
        .iter()
        .filter(|path| {
            fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .any(|line| {
                    line.trim()
                        .replace(' ', "")
                        .starts_with("#![allow(deprecated")
                })
        })
        .map(|path| {
            path.strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string()
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "crate-level allow(deprecated): {offenders:?}"
    );
}
