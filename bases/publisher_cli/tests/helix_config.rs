//! The repo's Helix configuration runs the language server that this
//! checkout builds, against a site that exists. A wrong site directory (it
//! once said `demo/`) or a binary from PATH (a stale build) gives an editor
//! without Presemble.

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
fn helix_runs_the_lsp_on_a_site_directory_that_exists() {
    let section = presemble_lsp_section();
    let args = quoted_values(&section, "args");
    assert_eq!(args.first().map(String::as_str), Some("lsp"), "{section}");
    let site = args
        .get(1)
        .unwrap_or_else(|| panic!("a site argument in {section}"));
    let dir = repo_root().join(site);
    assert!(
        dir.join("schemas").is_dir(),
        "the site directory {site} must exist and have schemas/: {}",
        dir.display()
    );
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
