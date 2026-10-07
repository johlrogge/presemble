//! String-level regression net for ADR-059 stage 5b.1: a lone link or image
//! is a paragraph, and its shape decides which slot takes it. These use only
//! `parse_and_assign`, `serialize_document`, `validate` and the element
//! variants that exist before the stage, so the same file runs against the
//! code from before it (where the `link_*`/`image_*`/`sole_*` tests fail) and
//! after it (where they pass).

use content::{
    AsPlainText, ContentElement, Severity, parse_and_assign, serialize_document, validate,
};
use schema::Grammar;

const SCHEMA: &str = "# Title {#title}\noccurs\n: exactly once\n\n\
     [Author](/author/<name>) {#author}\noccurs\n: exactly once\n\n\
     ![Cover](images/*.jpg) {#cover}\noccurs\n: exactly once\n\n\
     ----\n\nBody.\n";

fn grammar() -> Grammar {
    schema::parse_schema(SCHEMA).expect("schema parses")
}

const DOC: &str = "# T\n\n[Jo](/author/jo)\n\n![A cat](images/cat.jpg)\n\n----\n\nBody.\n";

/// The first element of the slot called `name`.
fn slot_element(doc: &content::Document, name: &str) -> ContentElement {
    doc.preamble
        .iter()
        .find(|s| s.name.as_str() == name)
        .and_then(|s| s.elements.front())
        .map(|e| e.node.clone())
        .unwrap_or_else(|| panic!("no element in slot {name}: {doc:#?}"))
}

fn messages(doc: &content::Document) -> Vec<String> {
    validate(doc, &grammar())
        .diagnostics
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.message)
        .collect()
}

/// L6: a link's text keeps its formatting through a save. (It used to be
/// stored decoded, so `[**a**](/x)` was saved as `[a](/x)`.)
#[test]
fn link_block_text_keeps_markup_l6() {
    let src = "# T\n\n[**a** link](/author/x)\n\n![c](images/c.jpg)\n\n----\n\nBody.\n";
    let doc = parse_and_assign(src, &grammar()).expect("parses");
    let saved = serialize_document(&doc);
    assert!(saved.contains("[**a** link](/author/x)"), "saved:\n{saved}");
}

/// L6: an image's alt text keeps its formatting through a save.
#[test]
fn image_alt_keeps_markup() {
    let src = "# T\n\n[Jo](/author/jo)\n\n![*a* cat](images/c.jpg)\n\n----\n\nBody.\n";
    let doc = parse_and_assign(src, &grammar()).expect("parses");
    let saved = serialize_document(&doc);
    assert!(saved.contains("![*a* cat](images/c.jpg)"), "saved:\n{saved}");
}

/// A lone link is a paragraph (its shape is what makes the link slot take it).
#[test]
fn sole_link_paragraph_fills_link_slot() {
    let doc = parse_and_assign(DOC, &grammar()).expect("parses");
    assert!(
        matches!(slot_element(&doc, "author"), ContentElement::Paragraph { .. }),
        "{:?}",
        slot_element(&doc, "author")
    );
    assert!(messages(&doc).is_empty(), "{:?}", messages(&doc));
}

/// A lone image is a paragraph (its shape is what makes the image slot take it).
#[test]
fn sole_image_paragraph_fills_image_slot() {
    let doc = parse_and_assign(DOC, &grammar()).expect("parses");
    assert!(
        matches!(slot_element(&doc, "cover"), ContentElement::Paragraph { .. }),
        "{:?}",
        slot_element(&doc, "cover")
    );
}

/// A link slot's paragraph that is no longer a lone link (text was added to
/// it in the store) is an Error that says what the slot expects. It is still
/// a paragraph and still saves; it is reassigned on reload.
#[test]
fn link_slot_paragraph_with_added_text_is_error_not_structural() {
    let mut doc = parse_and_assign(DOC, &grammar()).expect("parses");
    let slot = doc.preamble.iter_mut().find(|s| s.name.as_str() == "author").unwrap();
    slot.elements.front_mut().unwrap().node =
        ContentElement::paragraph_from_markdown("[Jo](/author/jo) and more");

    let errors = messages(&doc);
    assert!(
        errors.iter().any(|m| m.contains("slot 'author'") && m.contains("expected a link")),
        "no 'expected a link' error: {errors:?}"
    );
    // Editable and saveable.
    let saved = serialize_document(&doc);
    assert!(saved.contains("[Jo](/author/jo) and more"), "saved:\n{saved}");
    // Reassigned on reload: the paragraph is no longer in the link slot.
    let again = parse_and_assign(&saved, &grammar()).expect("parses");
    let author = again.preamble.iter().find(|s| s.name.as_str() == "author");
    assert!(
        !author.is_some_and(|s| s.elements.iter().any(|e| {
            matches!(&e.node, ContentElement::Paragraph { content, .. }
                if content.plain_text().contains("and more"))
        })),
        "{again:#?}"
    );
}

/// L15: inline HTML in a lone link's text is reported (it used to be dropped
/// on load, so nothing saw it).
#[test]
fn inline_html_in_link_text_is_error_l15() {
    let src = "# T\n\n[a <b>x</b>](/author/x)\n\n![c](images/c.jpg)\n\n----\n\nBody.\n";
    let doc = parse_and_assign(src, &grammar()).expect("parses");
    let errors = messages(&doc);
    assert!(errors.iter().any(|m| m.contains("inline HTML")), "no inline-HTML error: {errors:?}");
}
