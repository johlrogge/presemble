//! End-to-end tests for `presemble fmt` (ADR-070 decision 3): every file of a
//! kind that has a canonical writer is written the way a save would write it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use tempfile::TempDir;

const SITE_NAME: &str = "blog";
const POST: &str = "content/post/hello-world.md";
/// An ATX heading of the post, and the same heading written setext: same
/// meaning, not the canonical spelling.
const ATX: &str = "## What to Edit";
const SETEXT: &str = "What to Edit\n------------";

/// A scratch copy of `example-sites/blog` at `<tmp>/blog`, with its hiccup
/// templates moved up as `just site-build-example` does, so it builds.
fn scratch_blog() -> (TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../example-sites")
        .join(SITE_NAME);
    let tmp = TempDir::new().expect("tempdir");
    let site = tmp.path().join(SITE_NAME);
    copy_dir(&src, &site);
    let hiccup = site.join("templates/hiccup");
    if hiccup.is_dir() {
        copy_dir(&hiccup, &site.join("templates"));
        fs::remove_dir_all(&hiccup).expect("remove templates/hiccup");
    }
    (tmp, site)
}

fn copy_dir(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).expect("create dir");
    for entry in fs::read_dir(src).expect("read dir").flatten() {
        let to = dest.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), &to).expect("copy file");
        }
    }
}

fn presemble(args: &[&str], site: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_presemble"))
        .args(args)
        .arg(site)
        .output()
        .expect("run presemble")
}

fn fmt(site: &Path) -> Output {
    presemble(&["fmt"], site)
}

fn fmt_check(site: &Path) -> Output {
    presemble(&["fmt", "--check"], site)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Run fmt once so every file is canonical.
fn formatted(site: &Path) {
    let out = fmt(site);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
}

/// Write the post's heading setext: not canonical, same meaning.
fn unformat_post(site: &Path) {
    let path = site.join(POST);
    let text = fs::read_to_string(&path).expect("read post");
    assert!(text.contains(ATX), "the post has {ATX:?}:\n{text}");
    fs::write(&path, text.replacen(ATX, SETEXT, 1)).expect("write post");
}

/// Every file under `dir`: its bytes and its modification time.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut files = BTreeMap::new();
    collect(dir, dir, &mut files);
    files
}

fn collect(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, SystemTime)>) {
    for entry in fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, out);
        } else {
            let meta = fs::metadata(&path).expect("metadata");
            let rel = path.strip_prefix(base).expect("under base").to_path_buf();
            out.insert(
                rel,
                (
                    fs::read(&path).expect("read"),
                    meta.modified().expect("mtime"),
                ),
            );
        }
    }
}

#[test]
fn fmt_rewrites_only_files_whose_text_changes() {
    let (_tmp, site) = scratch_blog();
    formatted(&site);
    unformat_post(&site);
    let before = snapshot(&site);

    let out = fmt(&site);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
    let printed = stdout(&out);
    let listed: Vec<&str> = printed
        .lines()
        .filter(|l| l.starts_with("content/"))
        .collect();
    assert_eq!(listed, vec![POST], "stdout:\n{printed}");
    assert!(
        printed.contains("1 files formatted") && printed.contains(", 0 errors"),
        "stdout:\n{printed}"
    );
    let post = fs::read_to_string(site.join(POST)).expect("read post");
    assert!(
        post.contains(ATX) && !post.contains(SETEXT),
        "the post is canonical:\n{post}"
    );
    let after = snapshot(&site);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "no file is added or removed"
    );
    for (path, old) in &before {
        if path != Path::new(POST) {
            assert_eq!(&after[path], old, "{} is untouched", path.display());
        }
    }
}

#[test]
fn fmt_check_lists_the_files_and_exits_1() {
    let (_tmp, site) = scratch_blog();
    formatted(&site);
    unformat_post(&site);
    let before = snapshot(&site);

    let out = fmt_check(&site);

    assert_eq!(out.status.code(), Some(1), "stderr:\n{}", stderr(&out));
    let printed = stdout(&out);
    assert!(
        printed.contains(&format!("{POST}: would be reformatted")),
        "stdout:\n{printed}"
    );
    assert!(printed.contains(", 0 errors"), "stdout:\n{printed}");
    assert_eq!(snapshot(&site), before, "--check writes nothing");
}

#[test]
fn fmt_check_on_a_formatted_site_exits_0() {
    let (_tmp, site) = scratch_blog();
    formatted(&site);

    let out = fmt_check(&site);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
    let printed = stdout(&out);
    assert!(
        !printed
            .lines()
            .any(|l| l.ends_with(": would be reformatted")),
        "no file is listed:\n{printed}"
    );
    assert!(
        printed.contains("0 files would be reformatted") && printed.contains(", 0 errors"),
        "stdout:\n{printed}"
    );
}

#[test]
fn fmt_reports_a_file_without_a_schema_and_exits_2() {
    let (_tmp, site) = scratch_blog();
    formatted(&site);
    unformat_post(&site);
    let stray = site.join("content/nosuch/stray.md");
    fs::create_dir_all(stray.parent().expect("parent")).expect("create dir");
    fs::write(&stray, "# Stray\n").expect("write stray");

    // --check: the error wins over the change it found (2, not 1).
    let out = fmt_check(&site);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", stderr(&out));
    assert!(
        stderr(&out).contains("content/nosuch/stray.md: error:"),
        "stderr:\n{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).contains(&format!("{POST}: would be reformatted")),
        "the other files are still checked:\n{}",
        stdout(&out)
    );
    assert!(stdout(&out).contains(", 1 errors"), "{}", stdout(&out));

    // Writing: the other files are still formatted.
    let out = fmt(&site);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", stderr(&out));
    let post = fs::read_to_string(site.join(POST)).expect("read post");
    assert!(post.contains(ATX), "the post is formatted:\n{post}");
    assert_eq!(
        fs::read_to_string(&stray).expect("read stray"),
        "# Stray\n",
        "the file in error is left alone"
    );
}

#[test]
fn an_unreadable_content_file_is_reported() {
    let (_tmp, site) = scratch_blog();
    formatted(&site);
    unformat_post(&site);
    let binary = site.join("content/post/binary.md");
    let bytes = [b'#', b' ', 0xff, 0xfe, b'\n'];
    fs::write(&binary, bytes).expect("write binary");

    let out = fmt(&site);

    assert_eq!(out.status.code(), Some(2), "stderr:\n{}", stderr(&out));
    assert!(
        stderr(&out).contains("content/post/binary.md: error:"),
        "stderr:\n{}",
        stderr(&out)
    );
    let post = fs::read_to_string(site.join(POST)).expect("read post");
    assert!(post.contains(ATX), "the other files are formatted:\n{post}");
    assert_eq!(fs::read(&binary).expect("read binary"), bytes, "left alone");
}

#[test]
fn fmt_leaves_kinds_without_a_formatter_alone() {
    let (_tmp, site) = scratch_blog();
    // Files of every other kind, none of them in a canonical spelling.
    let others = [
        ("templates/nav.hiccup", "\n;; a note   \n"),
        ("schemas/post/item.md", "\n<!-- a note -->\n\n\n"),
        ("assets/style.css", "\na{ }   \n"),
        (".presemble/notes.txt", "*  not markdown  \n"),
    ];
    for (rel, extra) in others {
        let path = site.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("create dir");
        let mut text = fs::read_to_string(&path).unwrap_or_default();
        text.push_str(extra);
        fs::write(&path, text).expect("write");
    }
    unformat_post(&site);
    let before = snapshot(&site);

    let out = fmt(&site);

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
    let after = snapshot(&site);
    for (path, old) in &before {
        if !path.starts_with("content") {
            assert_eq!(&after[path], old, "{} is untouched", path.display());
        }
    }
    let printed = format!("{}{}", stdout(&out), stderr(&out));
    for (rel, _) in others {
        assert!(!printed.contains(rel), "{rel} is not visited:\n{printed}");
    }
}

#[test]
fn fmt_then_build_publishes_the_same_output() {
    let (tmp, site) = scratch_blog();
    unformat_post(&site);
    let output = tmp.path().join("output").join(SITE_NAME);

    let out = presemble(&["build"], &site);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
    let published: BTreeMap<PathBuf, Vec<u8>> = snapshot(&output)
        .into_iter()
        .map(|(path, (bytes, _))| (path, bytes))
        .collect();
    fs::remove_dir_all(&output).expect("remove output");

    let out = fmt(&site);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
    assert!(
        stdout(&out).contains(POST),
        "fmt changed the post:\n{}",
        stdout(&out)
    );

    let out = presemble(&["build"], &site);
    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", stderr(&out));
    let again: BTreeMap<PathBuf, Vec<u8>> = snapshot(&output)
        .into_iter()
        .map(|(path, (bytes, _))| (path, bytes))
        .collect();
    assert!(!published.is_empty(), "the site was published");
    assert_eq!(
        published.keys().collect::<Vec<_>>(),
        again.keys().collect::<Vec<_>>()
    );
    for (path, bytes) in &published {
        assert!(
            &again[path] == bytes,
            "{} differs after fmt",
            path.display()
        );
    }
}
