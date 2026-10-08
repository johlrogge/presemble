//! A gate for structure mode: every schema page of `site/` and each of
//! `example-sites/`, rendered in schema mode with the site's own templates, is
//! byte-identical to what it rendered when the fixture was captured. Structure
//! mode renders suggestion placeholders (an element with `data-presemble-hint`)
//! for empty slots; the snapshots of published and editorial output do not
//! cover it.
//!
//! The fixture holds one FNV-1a hash per schema page. When the sites or the
//! renderer change on purpose, refresh it: the failure message lists the new
//! hashes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED: &str = include_str!("fixtures/schema_mode_hashes.txt");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_tree(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap().flatten() {
        let name = entry.file_name();
        if name == "output" || name == ".presemble" || name == ".devenv" {
            continue;
        }
        let to = dest.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// FNV-1a, 64 bit.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// The schema URLs of a site: `/_schema/index`, `/_schema/<stem>/item`,
/// `/_schema/<stem>/index`, one per schema file.
fn schema_urls(schemas: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            schema_urls(schemas, &path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
            let rel = path.strip_prefix(schemas).unwrap().with_extension("");
            out.push(format!("/_schema/{}", rel.to_string_lossy().replace('\\', "/")));
        }
    }
}

fn render(name: &str, src: &Path, hiccup_templates: bool) -> Vec<String> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let site = tmp.path().join(name);
    copy_tree(src, &site);
    if hiccup_templates {
        let hiccup = site.join("templates/hiccup");
        if hiccup.is_dir() {
            copy_tree(&hiccup, &site.join("templates"));
            fs::remove_dir_all(&hiccup).unwrap();
        }
        let _ = fs::remove_dir_all(site.join("templates/html"));
    }
    let conductor = conductor::Conductor::new(site.clone()).expect("conductor loads the site");
    let mut urls = Vec::new();
    schema_urls(&site.join("schemas"), &site.join("schemas"), &mut urls);
    urls.sort();
    let mut hashes = BTreeMap::new();
    for url in urls {
        let result = conductor.handle_command(conductor::Command::RenderPage {
            path: url.clone(),
            mode: conductor::RenderMode::Schema,
        });
        let line = match result.response {
            conductor::Response::PageRendered { html } => {
                assert!(!html.contains("<presemble:"), "{name}{url} holds a presemble: element");
                format!("{:016x}", fnv1a(html.as_bytes()))
            }
            other => format!("NOT-RENDERED {other:?}"),
        };
        hashes.insert(url, line);
    }
    hashes.into_iter().map(|(url, hash)| format!("{name}{url} {hash}")).collect()
}

#[test]
fn schema_mode_renders_unchanged() {
    let root = repo_root();
    let mut actual: Vec<String> = render("site", &root.join("site"), false);
    for example in ["blog", "personal", "portfolio"] {
        actual.extend(render(example, &root.join("example-sites").join(example), true));
    }
    let actual = actual.join("\n") + "\n";
    assert_eq!(actual, EXPECTED, "schema mode output changed; the new hashes:\n{actual}");
}
