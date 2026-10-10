//! No rendering path emits a template element. `presemble:*` elements are
//! instructions to the renderer; a path that forgets to resolve one would leak
//! it into the page. Every page that `site/` and each of `example-sites/`
//! render, published and editorial, is free of them.

use std::fs;
use std::path::{Path, PathBuf};

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

fn html_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            html_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "html") {
            out.push(path);
        }
    }
}

fn assert_no_placeholders(name: &str, mode: &str, output: &Path) {
    let mut files = Vec::new();
    html_files(output, &mut files);
    assert!(!files.is_empty(), "{name} ({mode}) rendered no pages");
    for file in files {
        let html = fs::read_to_string(&file).unwrap();
        assert!(
            !html.contains("<presemble:"),
            "{name} ({mode}): {} holds a presemble: element",
            file.display()
        );
    }
}

fn check(name: &str, src: &Path, hiccup_templates: bool) {
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
    let url = publisher_cli::UrlConfig::default();
    publisher_cli::build_for_publish(&site, &url).expect("publishes");
    assert_no_placeholders(name, "published", &publisher_cli::output_dir(&site));
    publisher_cli::build_for_serve(&site, &url).expect("builds editorial");
    assert_no_placeholders(
        name,
        "editorial",
        &publisher_cli::editorial_output_dir(&site),
    );
}

#[test]
fn no_rendering_path_emits_a_placeholder_element() {
    let root = repo_root();
    check("site", &root.join("site"), false);
    for example in ["blog", "personal", "portfolio"] {
        check(example, &root.join("example-sites").join(example), true);
    }
}
