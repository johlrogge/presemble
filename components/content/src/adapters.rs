use std::io;
use std::path::Path;

use schema::Span;
use crate::diff::{Change, DocumentDiff};
use crate::document::Document;
use crate::serializer::{self, serialize_document};

/// A targeted text replacement in source coordinates.
#[derive(Debug, Clone)]
pub struct SourceEdit {
    /// Byte range in the original source to replace.
    pub span: Span,
    /// The replacement text.
    pub new_text: String,
}

/// Convert a DocumentDiff into targeted source edits.
///
/// Returns a Vec of SourceEdit that, when applied to `src` in reverse order,
/// produce the serialized form of the "after" document.
///
/// Falls back to full-document replacement for complex changes (SlotAdded,
/// SlotRemoved, SeparatorAdded, SeparatorRemoved) where insertion point
/// calculation is tricky.
pub fn diff_to_source_edits(
    src: &str,
    _before: &Document,
    after: &Document,
    diff: &DocumentDiff,
) -> Vec<SourceEdit> {
    if diff.is_empty() {
        return Vec::new();
    }

    // Check whether any change requires a full-document fallback.
    let needs_full_replacement = diff.changes.iter().any(|c| {
        matches!(
            c,
            Change::SlotAdded { .. }
                | Change::SlotRemoved { .. }
                | Change::SeparatorAdded
                | Change::SeparatorRemoved
        )
    });

    if needs_full_replacement {
        let new_text = serialize_document(after);
        return vec![SourceEdit {
            span: Span { start: 0, end: src.len() },
            new_text,
        }];
    }

    let mut edits = Vec::new();

    for change in &diff.changes {
        match change {
            Change::SlotChanged { before: before_elems, after: after_elems, .. } => {
                if before_elems.is_empty() {
                    // No before span to replace — use full replacement as safety fallback
                    let new_text = serialize_document(after);
                    return vec![SourceEdit {
                        span: Span { start: 0, end: src.len() },
                        new_text,
                    }];
                }

                let edit = make_elements_edit(src, before_elems, after_elems, after);
                edits.push(edit);
            }

            Change::BodyChanged { before: before_elems, after: after_elems, .. } => {
                if before_elems.is_empty() {
                    // No before span to replace — full replacement fallback
                    let new_text = serialize_document(after);
                    return vec![SourceEdit {
                        span: Span { start: 0, end: src.len() },
                        new_text,
                    }];
                }

                let edit = make_elements_edit(src, before_elems, after_elems, after);
                edits.push(edit);
            }

            // These are handled by the full-replacement path above.
            Change::SlotAdded { .. }
            | Change::SlotRemoved { .. }
            | Change::SeparatorAdded
            | Change::SeparatorRemoved => unreachable!("already handled by needs_full_replacement"),
        }
    }

    edits
}

/// Build a SourceEdit that replaces the source region covered by `before_elems`
/// with the serialized form of `after_elems`.
///
/// The strategy: replace exactly the region from the first element's start to
/// the last element's end. Preserve the trailing whitespace (e.g. `\n`) that the
/// original element ends with by appending the same trailing bytes to the new text.
fn make_elements_edit(
    src: &str,
    before_elems: &im::Vector<schema::Spanned<crate::document::ContentElement>>,
    after_elems: &im::Vector<schema::Spanned<crate::document::ContentElement>>,
    _after_doc: &Document,
) -> SourceEdit {
    let first = &before_elems[0];
    let last = &before_elems[before_elems.len() - 1];

    let span_start = first.span.start;
    let span_end = last.span.end;

    // Determine what trailing whitespace the original element ends with,
    // so we can append the same suffix to the new text.
    // This preserves inter-element blank lines that live just outside the span.
    let original_tail = trailing_whitespace_in_span(src, span_start, span_end);

    let new_text = if after_elems.is_empty() {
        // Elements removed: produce empty string (the span will be deleted).
        String::new()
    } else {
        // Check whether this is the last element in the document (body or slot).
        // If it's the last element and at the end of src, use serialize_document
        // for the trailing \n, otherwise use serialize_elements.
        let serialized = serializer::serialize_elements(after_elems);
        if original_tail.is_empty() {
            // No trailing whitespace in the original span — just use raw serialized form.
            // If this is the very end of the document (span_end == src.len()), we need
            // to add the final newline that serialize_document would add.
            if span_end == src.len() || all_whitespace_after(src, span_end) {
                format!("{serialized}\n")
            } else {
                serialized
            }
        } else {
            // Append the same trailing whitespace that the original element had.
            format!("{serialized}{original_tail}")
        }
    };

    SourceEdit {
        span: Span { start: span_start, end: span_end },
        new_text,
    }
}

/// Return the trailing whitespace bytes at the end of `src[start..end]`.
///
/// For example, if the span covers `"# hello\n"`, this returns `"\n"`.
/// If the span covers `"hello"`, this returns `""`.
fn trailing_whitespace_in_span(src: &str, start: usize, end: usize) -> &str {
    if start >= end || end > src.len() {
        return "";
    }
    let slice = &src[start..end];
    let trimmed_len = slice.trim_end_matches(['\n', '\r', ' ', '\t']).len();
    &slice[trimmed_len..]
}

/// Return true if all bytes at or after `pos` in `src` are whitespace.
fn all_whitespace_after(src: &str, pos: usize) -> bool {
    src[pos..].bytes().all(|b| matches!(b, b'\n' | b'\r' | b' ' | b'\t'))
}

// ---------------------------------------------------------------------------
// File writer adapter
// ---------------------------------------------------------------------------

/// Writes a Document to a file.
pub trait FileWriter {
    fn write(&self, path: &Path, doc: &Document) -> io::Result<()>;
}

/// Writes the full serialized document to disk.
pub struct FullDocumentWriter;

impl FileWriter for FullDocumentWriter {
    fn write(&self, path: &Path, doc: &Document) -> io::Result<()> {
        std::fs::write(path, serialize_document(doc))
    }
}

// ---------------------------------------------------------------------------
// Browser adapter (stub — completed in M5)
// ---------------------------------------------------------------------------

/// A DOM patch instruction for the browser editing client.
#[derive(Debug, Clone)]
pub enum DomPatch {
    ReplaceSlot { slot_name: String, html: String },
    InsertSlot { slot_name: String, html: String },
    RemoveSlot { slot_name: String },
    ReplaceBody { html: String },
    InsertSeparator,
    RemoveSeparator,
}

/// Convert a DocumentDiff into browser DOM patches.
///
/// Stub — returns empty. Implemented in M5.
pub fn diff_to_dom_patches(_diff: &DocumentDiff) -> Vec<DomPatch> {
    Vec::new()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use schema::parse_schema;
    use schema::Grammar;
    use crate::parser::parse_and_assign;
    use crate::diff::diff;
    use crate::transform::{Capitalize, InsertSeparator, InsertSlot, Transform};

    fn post_grammar() -> Grammar {
        let schema_src = r#"# Post title {#title}
occurs
: exactly once
content
: capitalized

Summary paragraph. {#summary}
occurs
: 1..3

[<name>](/author/<name>) {#author}
occurs
: exactly once

----

Body content.
headings
: h3..h6
"#;
        parse_schema(schema_src).expect("post schema should parse")
    }

    /// Apply a list of source edits to `src` in reverse order to maintain byte offsets.
    fn apply_source_edits(src: &str, edits: &[SourceEdit]) -> String {
        let mut result = src.to_string();
        let mut sorted: Vec<_> = edits.to_vec();
        sorted.sort_by(|a, b| b.span.start.cmp(&a.span.start));
        for edit in &sorted {
            result.replace_range(edit.span.start..edit.span.end, &edit.new_text);
        }
        result
    }

    // ── empty_diff_produces_no_edits ─────────────────────────────────────────

    #[test]
    fn empty_diff_produces_no_edits() {
        let grammar = Arc::new(post_grammar());
        let src = "# Hello\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n\nBody text.\n";
        let doc = parse_and_assign(src, &grammar).unwrap();
        let cloned = doc.clone();
        let d = diff(&doc, &cloned);
        let edits = diff_to_source_edits(src, &doc, &cloned, &d);
        assert!(edits.is_empty(), "expected no edits for empty diff, got: {edits:?}");
    }

    // ── slot_changed_produces_targeted_edit ──────────────────────────────────

    #[test]
    fn slot_changed_produces_targeted_edit() {
        let grammar = Arc::new(post_grammar());
        let src = "# hello\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let d = diff(&before, &after);

        let edits = diff_to_source_edits(src, &before, &after, &d);

        // Should have exactly one edit (for the title)
        assert_eq!(edits.len(), 1, "expected 1 edit, got: {edits:?}");

        let edit = &edits[0];
        // The edit should NOT cover the entire document
        assert!(
            edit.span.end < src.len(),
            "edit span ({:?}) should not cover the entire document (len={})",
            edit.span,
            src.len()
        );
        // The new text should contain the capitalized heading
        assert!(
            edit.new_text.contains("# Hello"),
            "expected '# Hello' in new_text: {:?}",
            edit.new_text
        );
    }

    // ── roundtrip_slot_changed ───────────────────────────────────────────────

    #[test]
    fn roundtrip_slot_changed() {
        let grammar = Arc::new(post_grammar());
        let src = "# hello\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let d = diff(&before, &after);

        let edits = diff_to_source_edits(src, &before, &after, &d);
        let result = apply_source_edits(src, &edits);

        let expected = serialize_document(&after);
        assert_eq!(
            result, expected,
            "roundtrip mismatch:\n  got:      {result:?}\n  expected: {expected:?}"
        );
    }

    // ── separator_added_falls_back_to_full_replacement ───────────────────────

    #[test]
    fn separator_added_falls_back_to_full_replacement() {
        let grammar = Arc::new(post_grammar());
        let src = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let transform = InsertSeparator;
        let after = transform.apply(before.clone()).unwrap();
        let d = diff(&before, &after);

        let edits = diff_to_source_edits(src, &before, &after, &d);

        // SeparatorAdded -> full replacement
        assert_eq!(edits.len(), 1, "expected 1 full-replacement edit, got: {edits:?}");
        let edit = &edits[0];
        assert_eq!(edit.span.start, 0, "full replacement should start at 0");
        assert_eq!(edit.span.end, src.len(), "full replacement should end at src.len()");
        assert!(
            edit.new_text.contains("----"),
            "expected '----' in new_text: {:?}",
            edit.new_text
        );
    }

    // ── slot_added_falls_back_to_full_replacement ────────────────────────────

    #[test]
    fn slot_added_falls_back_to_full_replacement() {
        use schema::Span;
        use crate::document::DocumentSlot;

        let grammar = Arc::new(post_grammar());
        let src = "Summary.\n\n[Jo](/author/jo)\n\n----\n";
        let mut before = parse_and_assign(src, &grammar).unwrap();
        // Remove the "title" slot from before's preamble entirely
        before.preamble.retain(|s| s.name.as_str() != "title");
        // Build after with a title slot present
        let mut after = before.clone();
        let title_slot = DocumentSlot {
            name: grammar.preamble.iter().find(|s| s.name.as_str() == "title").unwrap().name.clone(),
            elements: im::vector![schema::Spanned {
                node: crate::document::ContentElement::Heading {
                    level: schema::HeadingLevel::new(1).unwrap(),
                    text: "New Title".to_string(),
                },
                span: Span { start: 0, end: 0 },
            }],
        };
        after.preamble.push_front(title_slot);

        let d = diff(&before, &after);
        let edits = diff_to_source_edits(src, &before, &after, &d);

        // SlotAdded -> full replacement
        assert_eq!(edits.len(), 1, "expected 1 full-replacement edit, got: {edits:?}");
        let edit = &edits[0];
        assert_eq!(edit.span.start, 0);
        assert_eq!(edit.span.end, src.len());
    }

    // ── body_changed_produces_targeted_edit ─────────────────────────────────

    #[test]
    fn body_changed_roundtrip() {
        let grammar = Arc::new(post_grammar());
        let src_before = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n\nOriginal body.\n";
        let src_after_expected = "# Title\n\nSummary.\n\n[Jo](/author/jo)\n\n----\n\nChanged body.\n";
        let before = parse_and_assign(src_before, &grammar).unwrap();
        let after = parse_and_assign(src_after_expected, &grammar).unwrap();
        let d = diff(&before, &after);

        let edits = diff_to_source_edits(src_before, &before, &after, &d);
        assert!(!edits.is_empty(), "expected at least one edit for body change");

        let result = apply_source_edits(src_before, &edits);
        let expected = serialize_document(&after);
        assert_eq!(
            result, expected,
            "body changed roundtrip mismatch:\n  got:      {result:?}\n  expected: {expected:?}"
        );
    }

    // ── slot_changed_edit_does_not_cover_whole_doc ───────────────────────────

    #[test]
    fn slot_changed_edit_is_smaller_than_full_doc() {
        let grammar = Arc::new(post_grammar());
        // Long document so the title is clearly a small fraction
        let src = "# hello world\n\nSome summary text here.\n\n[Author](/author/someone)\n\n----\n\nLots of body content that should not be touched.\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let transform = Capitalize::new(Arc::clone(&grammar), "title").unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let d = diff(&before, &after);

        let edits = diff_to_source_edits(src, &before, &after, &d);
        assert_eq!(edits.len(), 1);
        let edit = &edits[0];
        let edit_size = edit.span.end - edit.span.start;
        assert!(
            edit_size < src.len(),
            "edit size ({edit_size}) should be smaller than full doc ({} bytes)",
            src.len()
        );
    }

    // ── insert_slot_roundtrip ─────────────────────────────────────────────────

    #[test]
    fn insert_slot_roundtrip() {
        let grammar = Arc::new(post_grammar());
        // Document without a title
        let src = "Summary.\n\n[Jo](/author/jo)\n\n----\n";
        let before = parse_and_assign(src, &grammar).unwrap();
        let transform = InsertSlot::new(Arc::clone(&grammar), "title", "New Title".to_string()).unwrap();
        let after = transform.apply(before.clone()).unwrap();
        let d = diff(&before, &after);

        let edits = diff_to_source_edits(src, &before, &after, &d);
        let result = apply_source_edits(src, &edits);
        let expected = serialize_document(&after);

        assert_eq!(
            result, expected,
            "insert_slot roundtrip mismatch:\n  got:      {result:?}\n  expected: {expected:?}"
        );
    }
}
