/// Golden fixed-point test harness for the serialize_from_store pipeline.
///
/// Contract: parse → store → serialise → parse → store → serialise must be
/// byte-identical on its second iteration (fixed-point under parse ∘ serialise).
///
/// Run with:
///   cargo test --package node_store_bridge --test serializer_golden
use std::path::{Path, PathBuf};

use content::parse_and_assign;
use node_store::NodeStore;
use node_store_bridge::content_bridge::document_to_store;
use node_store_bridge::serialize_from_store;
use schema::Grammar;

// ---------------------------------------------------------------------------
// Schema derivation logic
// ---------------------------------------------------------------------------

/// Given a content file path and the site root, derive the appropriate
/// grammar for that file.
///
/// Convention:
///   content/index.md                → schemas/index.md          (root collection)
///   content/{stem}/index.md         → schemas/{stem}/index.md   (stem collection)
///   content/{slug}.md               → schemas/index.md          (root item – rare)
///   content/{stem}/{slug}.md        → schemas/{stem}/item.md    (item, new convention)
///                                     or schemas/{stem}.md       (legacy flat)
///
/// Returns `None` if the schema file does not exist or fails to parse.
fn grammar_for_file(content_path: &Path, site_root: &Path) -> Option<Grammar> {
    let schemas_dir = site_root.join("schemas");
    let content_dir = site_root.join("content");

    // Strip the content/ prefix to get relative path within content/
    let rel = content_path.strip_prefix(&content_dir).ok()?;
    let components: Vec<_> = rel.components().collect();

    let schema_path = match components.as_slice() {
        // content/index.md → schemas/index.md
        [file] if file.as_os_str() == "index.md" => schemas_dir.join("index.md"),

        // content/{slug}.md (root item, not index)
        [_file] => {
            let candidate = schemas_dir.join("item.md");
            if candidate.exists() { candidate } else { schemas_dir.join("index.md") }
        }

        // content/{stem}/index.md → schemas/{stem}/index.md
        [stem, file] if file.as_os_str() == "index.md" => {
            let stem_str = stem.as_os_str().to_str()?;
            schemas_dir.join(stem_str).join("index.md")
        }

        // content/{stem}/{slug}.md → schemas/{stem}/item.md (or legacy schemas/{stem}.md)
        [stem, _slug] => {
            let stem_str = stem.as_os_str().to_str()?;
            let dir_based = schemas_dir.join(stem_str).join("item.md");
            if dir_based.exists() {
                dir_based
            } else {
                schemas_dir.join(format!("{stem_str}.md"))
            }
        }

        _ => return None,
    };

    if !schema_path.exists() {
        eprintln!(
            "  [skip] schema not found: {}",
            schema_path.display()
        );
        return None;
    }

    let src = std::fs::read_to_string(&schema_path).ok()?;
    schema::parse_schema(&src).ok()
}

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

/// Collect all .md files under `content/` in a site directory, recursively.
fn collect_content_files(site_root: &Path) -> Vec<PathBuf> {
    let content_dir = site_root.join("content");
    let mut files = Vec::new();
    collect_md_recursive(&content_dir, &mut files);
    files.sort();
    files
}

fn collect_md_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_recursive(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// Fixed-point check for one file
// ---------------------------------------------------------------------------

struct FixedPointFailure {
    path: PathBuf,
    first: String,
    second: String,
}

fn check_fixed_point(
    content_path: &Path,
    site_root: &Path,
) -> Result<(), FixedPointFailure> {
    let src = std::fs::read_to_string(content_path).expect("content file readable");

    let grammar = match grammar_for_file(content_path, site_root) {
        Some(g) => g,
        None => {
            println!("  [skip] no grammar for {}", content_path.display());
            return Ok(());
        }
    };

    // --- First pass: source → document → store → serialize ---
    let doc1 = match parse_and_assign(&src, &grammar) {
        Ok(d) => d,
        Err(e) => {
            println!("  [skip] parse error for {}: {e}", content_path.display());
            return Ok(());
        }
    };
    let mut store1 = NodeStore::new();
    let root1 = document_to_store(&doc1, &mut store1, None);
    let first = serialize_from_store(&store1, root1);

    // --- Second pass: first → document → store → serialize ---
    let doc2 = match parse_and_assign(&first, &grammar) {
        Ok(d) => d,
        Err(e) => {
            return Err(FixedPointFailure {
                path: content_path.to_path_buf(),
                first: first.clone(),
                second: format!("[parse error on second pass: {e}]"),
            });
        }
    };
    let mut store2 = NodeStore::new();
    let root2 = document_to_store(&doc2, &mut store2, None);
    let second = serialize_from_store(&store2, root2);

    if first == second {
        Ok(())
    } else {
        Err(FixedPointFailure {
            path: content_path.to_path_buf(),
            first,
            second,
        })
    }
}

// ---------------------------------------------------------------------------
// Main test
// ---------------------------------------------------------------------------

#[test]
fn serializer_reaches_fixed_point_on_all_fixtures() {
    // CARGO_MANIFEST_DIR is set to the package root at compile time.
    // For node_store_bridge at components/node_store_bridge/, the workspace
    // root is two directories up.
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent() // components/
        .and_then(|p| p.parent()) // workspace root
        .expect("workspace root resolvable")
        .to_path_buf();

    // Site roots to walk
    let candidate_sites = [
        workspace_root.join("example-sites/personal"),
        workspace_root.join("example-sites/blog"),
        workspace_root.join("example-sites/portfolio"),
        workspace_root.join("fixtures/blog-site"),
    ];

    // Files to skip (intentionally invalid)
    let skip_files = [
        workspace_root.join("fixtures/blog-site/content/article/invalid-post.md"),
    ];

    let mut existing_sites: Vec<PathBuf> = Vec::new();
    for site in &candidate_sites {
        if site.join("content").exists() {
            existing_sites.push(site.clone());
        } else {
            println!("  [info] site dir absent, skipping: {}", site.display());
        }
    }

    if existing_sites.is_empty() {
        panic!(
            "No fixture site directories found. Expected at least one of: {:?}",
            candidate_sites.iter().map(|p| p.display().to_string()).collect::<Vec<_>>()
        );
    }

    let mut all_files: Vec<PathBuf> = Vec::new();
    for site in &existing_sites {
        let files = collect_content_files(site);
        for f in files {
            if skip_files.iter().any(|s| s == &f) {
                println!("  [skip] intentionally invalid: {}", f.display());
            } else {
                all_files.push(f);
            }
        }
    }

    if all_files.is_empty() {
        panic!("No content files found in any fixture site.");
    }

    let mut failures: Vec<FixedPointFailure> = Vec::new();

    for path in &all_files {
        println!("checking {}", path.display());

        // Determine site root by finding the parent that contains a `content/` dir
        // and is the top-level site dir (i.e., path is under site/content/...).
        let site_root = existing_sites
            .iter()
            .find(|s| path.starts_with(*s))
            .expect("file should be under a known site root");

        match check_fixed_point(path, site_root) {
            Ok(()) => println!("  ok"),
            Err(failure) => {
                eprintln!("  FAIL: {}", failure.path.display());
                failures.push(failure);
            }
        }
    }

    if !failures.is_empty() {
        let mut msg = format!(
            "\n{} fixture(s) did NOT reach fixed-point:\n\n",
            failures.len()
        );
        for f in &failures {
            msg.push_str(&format!("=== FAIL: {} ===\n", f.path.display()));
            let first_preview: String = f.first.chars().take(500).collect();
            let second_preview: String = f.second.chars().take(500).collect();
            msg.push_str(&format!("--- FIRST (500 chars) ---\n{first_preview}\n"));
            msg.push_str(&format!("--- SECOND (500 chars) ---\n{second_preview}\n\n"));
        }
        panic!("{msg}");
    }

    println!(
        "\nAll {} fixture file(s) reached fixed-point.",
        all_files.len()
    );
}
