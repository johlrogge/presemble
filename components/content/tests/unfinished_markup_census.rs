//! Census of markup that looks unfinished (UM-1.0), over the site content in
//! the repository: `site/content/` and `example-sites/*/content/`.
//!
//! A report, not a gate: it always passes. Each hit is one line, followed by
//! the totals per kind and per tree. The report is written to
//! `target/unfinished-markup-census.txt` and printed.
//!
//! Run with `just census-markup`.
//!
//! Line and column are approximate (report only): the flagged characters are
//! searched for inside the block's source span, starting at the excerpt.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use content::inline::{Inline, Inlines};
use content::{ContentElement, Hit, Unfinished, parse_document, unfinished_markup};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Every `*.md` under `dir`, sorted.
fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// The trees, in report order: `site/`, then each example site.
fn trees(root: &Path) -> Vec<(String, PathBuf)> {
    let mut trees = vec![("site/".to_string(), root.join("site/content"))];
    let mut examples: Vec<PathBuf> = std::fs::read_dir(root.join("example-sites"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    examples.sort();
    for example in examples {
        let content = example.join("content");
        if content.is_dir() {
            let name = example.file_name().unwrap_or_default().to_string_lossy();
            trees.push((format!("example-sites/{name}/"), content));
        }
    }
    trees
}

/// A run of inlines to check, and where it sits inside its top-level block
/// (`i1/b0`, the form of `data-presemble-path`; table cells as `h0`/`r1c2`).
struct Run<'a> {
    nested: String,
    inlines: &'a Inlines,
}

fn join(path: &str, step: &str) -> String {
    if path.is_empty() {
        step.to_string()
    } else {
        format!("{path}/{step}")
    }
}

/// Every inline run of `element`, descending into quotes and list items.
fn runs<'a>(element: &'a ContentElement, path: &str, out: &mut Vec<Run<'a>>) {
    match element {
        ContentElement::Heading { content, .. } | ContentElement::Paragraph { content, .. } => out
            .push(Run {
                nested: path.to_string(),
                inlines: content,
            }),
        ContentElement::Table { columns, rows } => {
            for (c, column) in columns.iter().enumerate() {
                out.push(Run {
                    nested: join(path, &format!("h{c}")),
                    inlines: &column.header,
                });
            }
            for (r, row) in rows.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    out.push(Run {
                        nested: join(path, &format!("r{r}c{c}")),
                        inlines: cell,
                    });
                }
            }
        }
        ContentElement::Blockquote { blocks } => {
            for (j, block) in blocks.elements.iter().enumerate() {
                runs(&block.node, &join(path, &format!("b{j}")), out);
            }
        }
        ContentElement::List { list } => {
            for (k, item) in list.items.iter().enumerate() {
                let item_path = join(path, &format!("i{k}"));
                for (j, block) in item.elements.iter().enumerate() {
                    runs(&block.node, &join(&item_path, &format!("b{j}")), out);
                }
            }
        }
        _ => {}
    }
}

/// The literal text at `inline_path`.
fn text_at<'a>(inlines: &'a Inlines, inline_path: &[usize]) -> Option<&'a str> {
    let (first, rest) = inline_path.split_first()?;
    match inlines.as_slice().get(*first)? {
        Inline::Text(s) if rest.is_empty() => Some(s),
        Inline::Emph(inner) | Inline::Strong(inner) => text_at(inner, rest),
        Inline::Link { text, .. } => text_at(text, rest),
        _ => None,
    }
}

fn kind_name(kind: Unfinished) -> &'static str {
    match kind {
        Unfinished::CodeSpan => "CodeSpan",
        Unfinished::LinkTarget => "LinkTarget",
        Unfinished::Image => "Image",
        Unfinished::Strong => "Strong",
    }
}

const KINDS: [Unfinished; 4] = [
    Unfinished::CodeSpan,
    Unfinished::LinkTarget,
    Unfinished::Image,
    Unfinished::Strong,
];

/// 1-based line and column (in chars) of byte `offset` in `source`.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

/// One report line for `hit`.
fn hit_line(
    file: &str,
    source: &str,
    span: (usize, usize),
    block: usize,
    run: &Run,
    hit: &Hit,
) -> String {
    let text = text_at(run.inlines, &hit.inline_path).unwrap_or("");
    let chars: Vec<char> = text.chars().collect();
    let flagged: String = chars[hit.chars.clone()].iter().collect();
    let from = hit.chars.start.saturating_sub(20);
    let to = (hit.chars.end + 20).min(chars.len());
    let excerpt: String = chars[from..to].iter().collect();
    let excerpt = excerpt.replace('\n', "\\n");

    // Search the flagged characters inside the block's source, starting at the
    // excerpt's text before the hit when it is found there.
    let block_src = &source[span.0..span.1];
    let lead: String = chars[from..hit.chars.start].iter().collect();
    let search_from = if lead.is_empty() {
        None
    } else {
        block_src.find(&lead)
    }
    .map(|p| p + lead.len())
    .unwrap_or(0);
    let location = block_src[search_from..]
        .find(&flagged)
        .map(|p| line_col(source, span.0 + search_from + p))
        .or_else(|| {
            block_src
                .find(&flagged)
                .map(|p| line_col(source, span.0 + p))
        });
    let location = match location {
        Some((line, col)) => format!("{line}:{col}"),
        None => "?:?".to_string(),
    };

    let block = if run.nested.is_empty() {
        format!("block {block}")
    } else {
        format!("block {block} {}", run.nested)
    };
    format!(
        "{file}:{location}  {}  {block}  inline_path={:?} chars={}..{}  `{excerpt}`",
        kind_name(hit.kind),
        hit.inline_path,
        hit.chars.start,
        hit.chars.end,
    )
}

#[test]
#[ignore = "a report, not a gate: run with `just census-markup`"]
fn unfinished_markup_census() {
    let root = workspace_root();
    let mut report = String::new();
    // tree -> kind -> count
    let mut totals: Vec<(String, BTreeMap<&'static str, usize>, usize, usize)> = Vec::new();

    for (tree, dir) in trees(&root) {
        let mut per_kind: BTreeMap<&'static str, usize> =
            KINDS.iter().map(|k| (kind_name(*k), 0)).collect();
        let files = markdown_files(&dir);
        // Inline runs checked, so a report of zero hits shows it looked.
        let mut checked = 0;
        for path in &files {
            let rel = path
                .strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string();
            let Ok(source) = std::fs::read_to_string(path) else {
                writeln!(report, "{rel}: unreadable").unwrap();
                continue;
            };
            let doc = match parse_document(&source) {
                Ok(doc) => doc,
                Err(e) => {
                    writeln!(report, "{rel}: does not parse: {e}").unwrap();
                    continue;
                }
            };
            for (index, block) in doc.elements.iter().enumerate() {
                let mut block_runs = Vec::new();
                runs(&block.node, "", &mut block_runs);
                for run in &block_runs {
                    checked += 1;
                    for hit in unfinished_markup(run.inlines) {
                        *per_kind.entry(kind_name(hit.kind)).or_default() += 1;
                        let span = (block.span.start, block.span.end);
                        writeln!(
                            report,
                            "{}",
                            hit_line(&rel, &source, span, index, run, &hit)
                        )
                        .unwrap();
                    }
                }
            }
        }
        totals.push((tree, per_kind, files.len(), checked));
    }

    writeln!(report).unwrap();
    writeln!(report, "Totals").unwrap();
    let mut all: BTreeMap<&'static str, usize> = KINDS.iter().map(|k| (kind_name(*k), 0)).collect();
    for (tree, per_kind, files, checked) in &totals {
        let sum: usize = per_kind.values().sum();
        let kinds: Vec<String> = KINDS
            .iter()
            .map(|k| format!("{}={}", kind_name(*k), per_kind[kind_name(*k)]))
            .collect();
        writeln!(
            report,
            "  {tree}  {files} files  {checked} inline runs  {sum} hits  {}",
            kinds.join(" ")
        )
        .unwrap();
        for (kind, n) in per_kind {
            *all.entry(kind).or_default() += n;
        }
    }
    let sum: usize = all.values().sum();
    let kinds: Vec<String> = KINDS
        .iter()
        .map(|k| format!("{}={}", kind_name(*k), all[kind_name(*k)]))
        .collect();
    writeln!(report, "  all  {sum} hits  {}", kinds.join(" ")).unwrap();

    let out = root.join("target/unfinished-markup-census.txt");
    std::fs::create_dir_all(out.parent().expect("target dir")).expect("target dir");
    std::fs::write(&out, &report).expect("write the report");
    println!("{report}");
    println!("written to {}", out.display());
}
