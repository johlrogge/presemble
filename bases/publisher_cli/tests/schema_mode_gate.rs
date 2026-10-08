//! A gate for structure mode: every schema page of `site/` and each of
//! `example-sites/`, rendered in schema mode with the site's own templates, is
//! byte-identical to what it rendered at 9d5433b, before an insert without
//! `as` placed its value as it is. Structure mode renders suggestion
//! placeholders (an element with `data-presemble-hint`) for empty slots; the
//! snapshots of published and editorial output do not cover it.
//!
//! The fixture holds one FNV-1a hash per schema page. When the sites change on
//! purpose, refresh it: the failure message lists the new hashes.

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

/// The page with the attributes of every tag in sorted order. The order in
/// which schema mode writes the constraint attributes of an element varies from
/// run to run (it follows a hash map); the attributes themselves do not.
fn sort_attributes(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tag = &rest[open..];
        // The end of the tag: the first `>` outside quotes.
        let mut quote: Option<char> = None;
        let mut end = tag.len();
        for (i, c) in tag.char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (None, '"') | (None, '\'') => quote = Some(c),
                (None, '>') => {
                    end = i + 1;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&sorted_tag(&tag[..end]));
        rest = &tag[end..];
    }
    out.push_str(rest);
    out
}

fn sorted_tag(tag: &str) -> String {
    let inner = tag.trim_start_matches('<').trim_end_matches('>');
    let self_closing = inner.ends_with('/');
    let inner = inner.trim_end_matches('/');
    // Split on whitespace outside quotes.
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                current.push(c);
            }
            (None, '"') | (None, '\'') => {
                quote = Some(c);
                current.push(c);
            }
            (None, c) if c.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            (_, c) => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() || tag.starts_with("</") || tag.starts_with("<!") {
        return tag.to_string();
    }
    let name = parts.remove(0);
    parts.sort();
    let mut out = format!("<{name}");
    for p in parts {
        out.push(' ');
        out.push_str(&p);
    }
    if self_closing {
        out.push_str(" /");
    }
    out.push('>');
    out
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
                assert!(!html.contains("<presemble:"), "{name}{url} holds a presemble: element");                format!("{:016x}", fnv1a(sort_attributes(&html).as_bytes()))
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
