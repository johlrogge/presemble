use content::{ContentElement, Document, parse_and_assign, parse_document, serialize_document};
use node_store::NodeStore;
use node_store_bridge::{
    content_bridge::{document_to_store, store_to_document},
    schema_bridge::{grammar_to_store, store_to_grammar},
    template_bridge::{store_to_template, template_to_store},
};
use schema::{Grammar, parse_schema};
use std::collections::HashMap;
use std::path::Path;
use template::{
    dom::serialize_nodes,
    parse_template_hiccup, parse_template_xml, serialize_to_hiccup,
};

// ---------------------------------------------------------------------------
// File walking
// ---------------------------------------------------------------------------

fn walk_files(dir: &Path, extension: &str) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                result.extend(walk_files(&path, extension));
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                result.push(path);
            }
        }
    }
    result
}

fn walk_templates(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                result.extend(walk_templates(&path));
            } else {
                let ext = path.extension().and_then(|e| e.to_str());
                if matches!(ext, Some("hiccup") | Some("html")) {
                    result.push(path);
                }
            }
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Stem extraction
// ---------------------------------------------------------------------------

/// Given a content path like `site/content/post/hello.md`,
/// extract the stem `post` (the immediate subdirectory under content/).
fn content_stem(content_dir: &Path, file_path: &Path) -> Option<String> {
    let rel = file_path.strip_prefix(content_dir).ok()?;
    rel.components().next().and_then(|c| {
        if let std::path::Component::Normal(s) = c {
            s.to_str().map(|s| s.to_string())
        } else {
            None
        }
    })
}

// ---------------------------------------------------------------------------
// Schema comparison
// ---------------------------------------------------------------------------

fn compare_grammars(original: &Grammar, reconstructed: &Grammar) -> Vec<String> {
    let mut diffs = Vec::new();

    if original.preamble.len() != reconstructed.preamble.len() {
        diffs.push(format!(
            "slot count differs (expected {}, got {})",
            original.preamble.len(),
            reconstructed.preamble.len()
        ));
        return diffs;
    }

    for (i, (orig, rec)) in original
        .preamble
        .iter()
        .zip(reconstructed.preamble.iter())
        .enumerate()
    {
        if orig.name.as_str() != rec.name.as_str() {
            diffs.push(format!(
                "slot {} name differs (expected '{}', got '{}')",
                i,
                orig.name.as_str(),
                rec.name.as_str()
            ));
        }
        if orig.constraints.len() != rec.constraints.len() {
            diffs.push(format!(
                "slot '{}' constraint count differs (expected {}, got {})",
                orig.name.as_str(),
                orig.constraints.len(),
                rec.constraints.len()
            ));
        }
    }

    match (&original.body, &reconstructed.body) {
        (Some(_), None) => diffs.push("body rules missing in reconstruction".to_string()),
        (None, Some(_)) => diffs.push("unexpected body rules in reconstruction".to_string()),
        _ => {}
    }

    diffs
}

// ---------------------------------------------------------------------------
// Round-trip implementations
// ---------------------------------------------------------------------------

struct SchemaResult {
    path: String,
    slot_count: usize,
    body_rules: usize,
    diffs: Vec<String>,
}

struct ContentResult {
    path: String,
    preamble_slots: usize,
    body_elements: usize,
    diffs: Vec<String>,
}

struct TemplateResult {
    path: String,
    node_count: usize,
    diffs: Vec<String>,
}

fn round_trip_schema(path: &Path, store: &mut NodeStore) -> SchemaResult {
    let display = path.display().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return SchemaResult {
                path: display,
                slot_count: 0,
                body_rules: 0,
                diffs: vec![format!("read error: {e}")],
            };
        }
    };

    let grammar = match parse_schema(&src) {
        Ok(g) => g,
        Err(e) => {
            return SchemaResult {
                path: display,
                slot_count: 0,
                body_rules: 0,
                diffs: vec![format!("parse error: {e:?}")],
            };
        }
    };

    let slot_count = grammar.preamble.len();
    let body_rules = if grammar.body.is_some() { 1 } else { 0 };

    let root = grammar_to_store(&grammar, store);
    let reconstructed = store_to_grammar(store, root);

    let diffs = compare_grammars(&grammar, &reconstructed);

    SchemaResult {
        path: display,
        slot_count,
        body_rules,
        diffs,
    }
}

fn round_trip_content(
    path: &Path,
    grammar: Option<&Grammar>,
    store: &mut NodeStore,
) -> ContentResult {
    let display = path.display().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return ContentResult {
                path: display,
                preamble_slots: 0,
                body_elements: 0,
                diffs: vec![format!("read error: {e}")],
            };
        }
    };

    // Parse the document — use parse_and_assign if we have a grammar, else parse_document.
    let doc = if let Some(g) = grammar {
        match parse_and_assign(&src, g) {
            Ok(d) => d,
            Err(e) => {
                return ContentResult {
                    path: display,
                    preamble_slots: 0,
                    body_elements: 0,
                    diffs: vec![format!("parse error: {e:?}")],
                };
            }
        }
    } else {
        match parse_document(&src) {
            Ok(flat) => {
                // FlatDocument has only elements; build a minimal Document with
                // no preamble and all elements as body.
                let has_separator = flat
                    .elements
                    .iter()
                    .any(|e| matches!(e.node, ContentElement::Separator));
                let body: im::Vector<_> = flat
                    .elements
                    .into_iter()
                    .filter(|e| !matches!(e.node, ContentElement::Separator))
                    .collect();
                Document {
                    preamble: im::Vector::new(),
                    body,
                    has_separator,
                    separator_span: None,
                }
            }
            Err(e) => {
                return ContentResult {
                    path: display,
                    preamble_slots: 0,
                    body_elements: 0,
                    diffs: vec![format!("parse error: {e:?}")],
                };
            }
        }
    };

    let preamble_slots = doc.preamble.len();
    let body_elements = doc.body.len();

    let root = document_to_store(&doc, store);
    let reconstructed = store_to_document(store, root);

    // Re-serialize both and compare text
    let original_serialized = serialize_document(&doc);
    let reconstructed_serialized = serialize_document(&reconstructed);

    let diffs = if original_serialized == reconstructed_serialized {
        vec![]
    } else {
        // Find first differing line for a useful message
        let orig_lines: Vec<&str> = original_serialized.lines().collect();
        let rec_lines: Vec<&str> = reconstructed_serialized.lines().collect();
        if orig_lines.len() != rec_lines.len() {
            vec![format!(
                "serialized line count differs (expected {}, got {})",
                orig_lines.len(),
                rec_lines.len()
            )]
        } else {
            orig_lines
                .iter()
                .zip(rec_lines.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| format!("line {} differs: expected {:?}, got {:?}", i + 1, a, b))
                .take(3)
                .collect()
        }
    };

    ContentResult {
        path: display,
        preamble_slots,
        body_elements,
        diffs,
    }
}

fn round_trip_template(path: &Path, store: &mut NodeStore) -> TemplateResult {
    let display = path.display().to_string();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return TemplateResult {
                path: display,
                node_count: 0,
                diffs: vec![format!("read error: {e}")],
            };
        }
    };

    let is_hiccup = path.extension().and_then(|e| e.to_str()) == Some("hiccup");

    let nodes = if is_hiccup {
        match parse_template_hiccup(&src) {
            Ok(n) => n,
            Err(e) => {
                return TemplateResult {
                    path: display,
                    node_count: 0,
                    diffs: vec![format!("parse error: {e:?}")],
                };
            }
        }
    } else {
        match parse_template_xml(&src) {
            Ok(n) => n,
            Err(e) => {
                return TemplateResult {
                    path: display,
                    node_count: 0,
                    diffs: vec![format!("parse error: {e:?}")],
                };
            }
        }
    };

    let node_count = nodes.len();

    let root_ids = template_to_store(&nodes, store);
    let reconstructed = store_to_template(store, &root_ids);

    // Re-serialize both and compare
    let original_serialized = if is_hiccup {
        serialize_to_hiccup(&nodes)
    } else {
        serialize_nodes(&nodes)
    };

    let reconstructed_serialized = if is_hiccup {
        serialize_to_hiccup(&reconstructed)
    } else {
        serialize_nodes(&reconstructed)
    };

    let diffs = if original_serialized == reconstructed_serialized {
        vec![]
    } else {
        let orig_lines: Vec<&str> = original_serialized.lines().collect();
        let rec_lines: Vec<&str> = reconstructed_serialized.lines().collect();
        if orig_lines.len() != rec_lines.len() {
            vec![format!(
                "serialized line count differs (expected {}, got {})",
                orig_lines.len(),
                rec_lines.len()
            )]
        } else {
            orig_lines
                .iter()
                .zip(rec_lines.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| {
                    format!("line {} differs: expected {:?}, got {:?}", i + 1, a, b)
                })
                .take(3)
                .collect()
        }
    };

    TemplateResult {
        path: display,
        node_count,
        diffs,
    }
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

pub fn run() {
    let site_dir = Path::new("site");

    println!("=== Node Store Round-Trip Test ===\n");

    let mut store = NodeStore::new();

    // ---- Schemas ------------------------------------------------------------
    println!("Schemas:");

    let schemas_dir = site_dir.join("schemas");
    let schema_paths = walk_files(&schemas_dir, "md");

    let mut grammars: HashMap<String, Grammar> = HashMap::new();

    for path in &schema_paths {
        let result = round_trip_schema(path, &mut store);
        if result.diffs.is_empty() {
            println!(
                "  [OK] {} ({} slots, {} body rules)",
                result.path, result.slot_count, result.body_rules
            );
        } else {
            println!("  [FAIL] {} -- MISMATCH:", result.path);
            for diff in &result.diffs {
                println!("         {diff}");
            }
        }

        // Also cache the parsed grammar for content parsing
        if let Ok(src) = std::fs::read_to_string(path)
            && let Ok(grammar) = parse_schema(&src)
            && let Ok(rel) = path.strip_prefix(&schemas_dir)
        {
            let stem_key = rel
                .with_extension("")
                .to_string_lossy()
                .replace('\\', "/");
            grammars.insert(stem_key, grammar);
        }
    }

    // ---- Content ------------------------------------------------------------
    println!("\nContent:");

    let content_dir = site_dir.join("content");
    let content_paths = walk_files(&content_dir, "md");

    for path in &content_paths {
        // Determine the grammar key for this content file.
        // e.g. site/content/post/hello.md -> stem "post", file name "hello"
        // -> look for grammar key "post/item" (most specific) or "post/index"
        let grammar = if let Some(stem) = content_stem(&content_dir, path) {
            let file_stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("item");

            // Try matching: <stem>/<file_stem>, then <stem>/item, then <stem>/index
            let candidates = [
                format!("{stem}/{file_stem}"),
                format!("{stem}/item"),
                format!("{stem}/index"),
            ];

            candidates
                .iter()
                .find_map(|key| grammars.get(key.as_str()))
        } else {
            // Top-level content file (e.g., site/content/index.md)
            grammars.get("index")
        };

        let result = round_trip_content(path, grammar, &mut store);
        if result.diffs.is_empty() {
            println!(
                "  [OK] {} ({} preamble slots, {} body elements)",
                result.path, result.preamble_slots, result.body_elements
            );
        } else {
            println!("  [FAIL] {} -- MISMATCH:", result.path);
            for diff in &result.diffs {
                println!("         {diff}");
            }
        }
    }

    // ---- Templates ----------------------------------------------------------
    println!("\nTemplates:");

    let templates_dir = site_dir.join("templates");
    let template_paths = walk_templates(&templates_dir);

    for path in &template_paths {
        let result = round_trip_template(path, &mut store);
        if result.diffs.is_empty() {
            println!("  [OK] {} ({} nodes)", result.path, result.node_count);
        } else {
            println!("  [FAIL] {} -- MISMATCH:", result.path);
            for diff in &result.diffs {
                println!("         {diff}");
            }
        }
    }

    // ---- Store stats --------------------------------------------------------
    println!("\nStore stats:");
    println!("  Nodes: {}", store.node_count());
    println!("  Edges: {}", store.edge_count());
    println!("  Interned names: {}", store.name_count());

    // ---- Duplicate analysis -------------------------------------------------
    println!("\nDuplicate analysis:");
    let mut text_values: HashMap<String, usize> = HashMap::new();
    let mut element_names: HashMap<String, usize> = HashMap::new();
    let mut text_count = 0usize;
    let mut element_count = 0usize;

    for (_id, node) in store.iter() {
        match node {
            node_store::Node::Text(s) => {
                text_count += 1;
                *text_values.entry(s.clone()).or_default() += 1;
            }
            node_store::Node::Element(name) => {
                element_count += 1;
                let name_str = store.resolve_name(*name).to_string();
                *element_names.entry(name_str).or_default() += 1;
            }
            _ => {}
        }
    }

    let unique_texts = text_values.len();
    let duplicate_texts = text_count - unique_texts;
    println!("  Text nodes: {text_count} total, {unique_texts} unique, {duplicate_texts} could be shared");

    // Top duplicated text values
    let mut top_dupes: Vec<_> = text_values.iter().filter(|(_, c)| **c > 1).collect();
    top_dupes.sort_by(|a, b| b.1.cmp(a.1));
    if !top_dupes.is_empty() {
        println!("  Top duplicated text values:");
        for (val, count) in top_dupes.iter().take(10) {
            let display = if val.len() > 40 { &val[..40] } else { val };
            println!("    {count}x \"{display}\"");
        }
    }

    // Element name frequency
    println!("\n  Element types ({element_count} total):");
    let mut name_freq: Vec<_> = element_names.iter().collect();
    name_freq.sort_by(|a, b| b.1.cmp(a.1));
    for (name, count) in name_freq.iter().take(15) {
        println!("    {count:>4}x {name}");
    }

    // ---- Impact resolution --------------------------------------------------
    println!("\nImpact resolution (samples):");
    // Find text nodes that have parents (are deep in the tree) and trace to roots
    let mut samples = 0;
    for (id, node) in store.iter() {
        if let node_store::Node::Text(s) = node {
            let parents = store.parents(id);
            if !parents.is_empty() && s.len() > 20 {
                let roots = store.impact_roots(id);
                let depth = {
                    let mut d = 0;
                    let mut current = id;
                    while let Some(&p) = store.parents(current).first() {
                        d += 1;
                        current = p;
                    }
                    d
                };
                let display = if s.len() > 50 { &s[..50] } else { s.as_str() };
                println!("  Text \"{display}...\" (depth {depth})");
                println!("    impacts {} root(s):", roots.len());
                for root_id in &roots {
                    if let Some(node_store::Node::Element(name)) = store.get(*root_id) {
                        // Find the document name by looking for a "name" attribute
                        let doc_name = store.attributes(*root_id)
                            .iter()
                            .find_map(|(n, vid)| {
                                if matches!(store.resolve_name(*n), "name" | "text")
                                    && let Some(node_store::Node::Text(s)) = store.get(*vid)
                                {
                                    Some(s.clone())
                                } else {
                                    None
                                }
                            });
                        if let Some(name_val) = doc_name {
                            println!("    -> {}(\"{}\")", store.resolve_name(*name), name_val);
                        } else {
                            println!("    -> Element(\"{}\")", store.resolve_name(*name));
                        }
                    }
                }
                samples += 1;
                if samples >= 3 {
                    break;
                }
            }
        }
    }
}
