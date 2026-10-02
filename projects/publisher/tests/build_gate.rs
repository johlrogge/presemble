//! End-to-end tests for the publish gate (ADR-061): `presemble build` refuses
//! to publish a site with validation errors.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const SITE_NAME: &str = "blog-site";

/// Copy `fixtures/blog-site` (without its `output/`) to `<tmp>/blog-site`, so
/// the build output lands in `<tmp>/output/blog-site`.
fn copy_fixture_site() -> (TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures").join(SITE_NAME);
    let tmp = TempDir::new().expect("tempdir");
    let dest = tmp.path().join(SITE_NAME);
    copy_dir_excluding(&src, &dest, "output");
    (tmp, dest)
}

fn copy_dir_excluding(src: &Path, dest: &Path, exclude: &str) {
    fs::create_dir_all(dest).expect("create dir");
    for entry in fs::read_dir(src).expect("read dir").flatten() {
        let name = entry.file_name();
        if name == exclude {
            continue;
        }
        let dest_path = dest.join(&name);
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_excluding(&entry.path(), &dest_path, exclude);
        } else {
            fs::copy(entry.path(), &dest_path).expect("copy file");
        }
    }
}

fn presemble_build(site: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_presemble"))
        .arg("build")
        .arg(site)
        .output()
        .expect("run presemble")
}

fn index_html(tmp: &TempDir) -> PathBuf {
    tmp.path().join("output").join(SITE_NAME).join("index.html")
}

#[test]
fn build_refuses_invalid_site_and_writes_nothing() {
    let (tmp, site) = copy_fixture_site();

    let out = presemble_build(&site);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(1), "stderr:\n{stderr}");
    assert!(stderr.contains("content/article/invalid-post.md:"), "stderr:\n{stderr}");
    assert!(stderr.contains(": error: "), "stderr:\n{stderr}");
    assert!(stderr.contains("site not published"), "stderr:\n{stderr}");
    assert!(!index_html(&tmp).exists(), "no page should be written");
}

#[test]
fn build_refusal_leaves_previous_output_untouched() {
    let (tmp, site) = copy_fixture_site();
    // Output from an earlier build, at `<tmp>/output/blog-site/`.
    let index = index_html(&tmp);
    let out_dir = index.parent().expect("output dir").to_path_buf();
    fs::create_dir_all(&out_dir).expect("create output dir");
    fs::write(&index, "previous").expect("write previous index");
    let stale = out_dir.join("stale.txt");
    fs::write(&stale, "stale").expect("write stale file");

    let out = presemble_build(&site);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(1), "stderr:\n{stderr}");
    assert_eq!(fs::read_to_string(&index).expect("read index"), "previous");
    assert!(stale.exists(), "earlier output must be left as it was");
}

#[test]
fn build_publishes_valid_site() {
    let (tmp, site) = copy_fixture_site();
    fs::remove_file(site.join("content/article/invalid-post.md")).expect("remove invalid post");

    let out = presemble_build(&site);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{stderr}");
    assert!(index_html(&tmp).exists(), "index.html should be written");
}
