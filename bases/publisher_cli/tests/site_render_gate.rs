//! A gate for the 5b series (ADR-059 stage 5b): every page that `site/` and
//! each of `example-sites/` render, published, with their own templates, is
//! byte-identical to what it rendered at b706bb8, before a lone link or image
//! became a paragraph. The fixture holds one FNV-1a hash per output file.
//!
//! When the sites' own content or templates change on purpose, refresh the
//! fixture: the failure message lists the new hashes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED: &str = include_str!("fixtures/site_render_hashes.txt");

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

fn collect(dir: &Path, base: &Path, out: &mut BTreeMap<String, u64>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect(&path, base, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel, fnv1a(&fs::read(&path).unwrap()));
        }
    }
}

/// Publish a copy of `src` and hash every file of its output. Example sites
/// keep their templates in `templates/hiccup/`; they are moved up, as
/// `just site-build-example` does.
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
    let published = publisher_cli::build_for_publish(&site, &publisher_cli::UrlConfig::default());
    let mut lines = Vec::new();
    match published {
        Ok(_) => {
            let out = publisher_cli::output_dir(&site);
            let mut hashes = BTreeMap::new();
            collect(&out, &out, &mut hashes);
            for (file, hash) in hashes {
                lines.push(format!("{name}/{file} {hash:016x}"));
            }
        }
        Err(e) => lines.push(format!("{name} NOT-PUBLISHABLE {e}")),
    }
    lines
}

#[test]
fn site_templates_render_unchanged() {
    let root = repo_root();
    let mut actual: Vec<String> = render("site", &root.join("site"), false);
    for example in ["blog", "personal", "portfolio"] {
        actual.extend(render(
            example,
            &root.join("example-sites").join(example),
            true,
        ));
    }
    let actual = actual.join("\n") + "\n";
    assert_eq!(
        actual, EXPECTED,
        "rendered output changed; the new hashes:\n{actual}"
    );
}
