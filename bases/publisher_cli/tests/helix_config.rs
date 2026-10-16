//! The repo's Helix configuration runs the language server that this
//! checkout builds, one per site: it names no site (the server finds it), and
//! Helix's workspace root for a file is the nearest directory holding
//! `schemas/`. A fixed site directory (it once said `demo/`, then `site/`) or
//! a binary from PATH (a stale build) gives an editor without Presemble.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `[language-server.presemble-lsp]` section of `.helix/languages.toml`.
fn presemble_lsp_section() -> String {
    let text = std::fs::read_to_string(repo_root().join(".helix/languages.toml"))
        .expect(".helix/languages.toml is readable");
    let start = text
        .find("[language-server.presemble-lsp]")
        .expect("a presemble-lsp server section");
    let rest = &text[start..];
    let end = rest[1..].find("\n[").map_or(rest.len(), |i| i + 1);
    rest[..end].to_string()
}

/// The quoted strings on the line `key = ...` of `section`.
fn quoted_values(section: &str, key: &str) -> Vec<String> {
    let line = section
        .lines()
        .find(|line| {
            line.trim_start()
                .strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with('='))
        })
        .unwrap_or_else(|| panic!("a `{key} =` line in {section}"));
    line.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

#[test]
fn helix_runs_the_lsp_without_naming_a_site() {
    let section = presemble_lsp_section();
    assert_eq!(
        quoted_values(&section, "args"),
        ["lsp"],
        "the server finds the site of the files it is given: {section}"
    );
}

/// The `[[language]]` entry of `.helix/languages.toml` named `name`.
fn language_section(name: &str) -> String {
    let text = std::fs::read_to_string(repo_root().join(".helix/languages.toml"))
        .expect(".helix/languages.toml is readable");
    text.split("[[language]]")
        .skip(1)
        .map(|entry| entry.split("\n[").next().unwrap_or(entry))
        .find(|entry| quoted_values(entry, "name") == [name])
        .unwrap_or_else(|| panic!("a [[language]] entry named {name}"))
        .to_string()
}

#[test]
fn helix_gives_each_site_its_own_workspace_root() {
    // The same roots for both, or a site's content and templates would get
    // different servers.
    for name in ["markdown", "html"] {
        let entry = language_section(name);
        assert!(
            quoted_values(&entry, "roots").contains(&"schemas".to_string()),
            "{name}: the nearest directory holding schemas/ is the root: {entry}"
        );
    }
}

#[test]
fn helix_runs_the_dev_build_of_this_checkout() {
    let section = presemble_lsp_section();
    assert_eq!(
        quoted_values(&section, "command"),
        ["target/debug/presemble"],
        "not a binary from PATH: {section}"
    );
}
