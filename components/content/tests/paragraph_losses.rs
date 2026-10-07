//! String-level regression net for the paragraph losses ADR-059 stage 5
//! fixes. These use only `parse_and_assign`, `serialize_document` and the
//! validator's report, so the same file runs against the code from before the
//! stage (where each of the `loss_*` tests fails) and after it (where they
//! pass). The model-level tests live next to the parser and serializer.

use content::{parse_and_assign, serialize_document, validate};
use schema::Grammar;

const SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
     Summary. {#summary}\noccurs\n: 0..3\n\n\
     [<name>](/post/<name>) {#latest}\noccurs\n: *\n\n\
     ----\n\nBody.\n";

fn grammar() -> Grammar {
    schema::parse_schema(SCHEMA).expect("schema parses")
}

/// The file written back after reading `src`.
fn saved(src: &str) -> String {
    serialize_document(&parse_and_assign(src, &grammar()).expect("parses"))
}

/// Loss 1: the text after a leading link was dropped.
#[test]
fn loss_a_leading_link_followed_by_text_keeps_the_text() {
    let src = "# T\n\n----\n\n[a](/x) trailing\n";
    assert_eq!(saved(src), src);
}

/// Loss 2: an image inside text was pulled out into a block after it.
#[test]
fn loss_image_inside_text_is_saved_as_one_paragraph() {
    let src = "# T\n\n----\n\nSee ![c](c.png) here.\n";
    assert_eq!(saved(src), src);
}

/// Loss 5 (L24): a reference definition that is used kept its `[a][r]` source
/// and lost the link on save.
#[test]
fn loss_a_used_reference_definition_keeps_its_link() {
    let src = "# T\n\n----\n\nSee [a][r].\n\n[r]: /x\n";
    let out = saved(src);
    assert!(out.contains("(/x)"), "the link is lost on save: {out:?}");
}

/// Loss 6 (L15): inline HTML in a paragraph that contains an image was
/// decoded away, so the validator did not see it.
#[test]
fn loss_inline_html_in_an_image_bearing_paragraph_is_diagnosed() {
    let g = grammar();
    let doc = parse_and_assign("# T\n\n----\n\nt <b>x</b> ![i](i.png)\n", &g).expect("parses");
    let messages: Vec<String> =
        validate(&doc, &g).diagnostics.into_iter().map(|d| d.message).collect();
    assert!(
        messages.iter().any(|m| m.contains("inline HTML")),
        "no inline-HTML diagnostic: {messages:?}"
    );
}

/// Guard: a marker on its own line after a link expression is misuse. It is
/// written as `\[x](->> :post) {#n}` (the text escaped, the marker structural),
/// and after a save and a reload it is still unassigned, still an Error, and
/// the marker has not bound the expression to a slot.
#[test]
fn own_line_marker_misuse_stays_misuse_after_a_save() {
    let g = grammar();
    let src = "# T\n\n[x](->> :post)\n{#n}\n\n----\n\nBody.\n";
    let doc = parse_and_assign(src, &g).expect("parses");
    assert_eq!(doc.unassigned.len(), 1, "{doc:#?}");
    assert!(!validate(&doc, &g).is_valid());

    let first = serialize_document(&doc);
    assert!(first.contains("\\[x](->> :post) {#n}"), "saved:\n{first}");

    let again = parse_and_assign(&first, &g).expect("parses");
    assert_eq!(again.unassigned.len(), 1, "still unassigned: {again:#?}");
    let latest = again.preamble.iter().find(|s| s.name.as_str() == "latest");
    assert!(latest.is_none_or(|s| s.elements.is_empty()), "the slot was bound: {again:#?}");
    let report = validate(&again, &g);
    assert!(!report.is_valid(), "the misuse Error is gone: {report:#?}");
    assert!(
        report.diagnostics.iter().any(|d| d.message.contains("{#n}")),
        "no diagnostic names the marker: {report:#?}"
    );
    // Stable from the first save on.
    assert_eq!(serialize_document(&again), first);
}
