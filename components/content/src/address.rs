//! Server-emitted block addresses (ADR-059 section 10).
//!
//! In editorial output every body block and slot element carries a
//! `data-presemble-address`: a NED selection that resolves to exactly that
//! block, or nothing when the block cannot be addressed exactly. The browser
//! sends it back as the `selection` of a block edit.
//!
//! The functions are pure over content elements, so the legacy `Document`
//! render path and the native `NodeStore` render path build the same address
//! for the same block.

use std::collections::HashMap;

use crate::document::ContentElement;
use crate::inline::AsPlainText;

/// Where a run of blocks lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressContainer<'a> {
    /// The document body.
    Body,
    /// A preamble slot, by name.
    Slot(&'a str),
}

/// The store kind name of a block: the element name `content_bridge` gives it.
pub fn kind_name(el: &ContentElement) -> &'static str {
    match el {
        ContentElement::Heading { .. } => "heading",
        ContentElement::Paragraph { .. } => "paragraph",
        ContentElement::CodeBlock { .. } => "code-block",
        ContentElement::Blockquote { .. } => "blockquote",
        ContentElement::List { .. } => "list",
        ContentElement::Table { .. } => "table",
        ContentElement::HtmlBlock { .. } => "html-block",
        ContentElement::Separator => "separator",
        ContentElement::LinkExpression { .. } => "link-expression",
    }
}

/// A Clojure string literal for `s`, with surrounding quotes. Escapes `\` and
/// `"`; strips CR and NUL.
pub fn clj_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\0' | '\r' => {}
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The expression for the document at `file`.
fn doc_expr(file: &str) -> String {
    format!("(ned/doc-by-path {})", clj_str(file))
}

/// The address of the preamble slot container `slot_name` of the document at
/// `file`: `(ned/slot (ned/doc-by-path "<file>") "<slot>")`. An empty slot is
/// addressed by it (a block edit with an empty base fills it, ADR-059 §10).
pub fn slot_address(file: &str, slot_name: &str) -> String {
    format!("(ned/slot {} {})", doc_expr(file), clj_str(slot_name))
}

/// The text of `doc-by-path` for an address built without a file. A render
/// path that does not know the file yet builds addresses with `""` and
/// fills it in later with [`with_file`].
const NO_FILE_DOC: &str = "(ned/doc-by-path \"\")";

/// Fill in the file of an address built with `file = ""`. `None` when the file
/// is still unknown, because an address without a file addresses nothing.
pub fn with_file(address: &str, file: &str) -> Option<String> {
    if file.is_empty() {
        return None;
    }
    Some(address.replacen(NO_FILE_DOC, &doc_expr(file), 1))
}

/// The address of each of `elements`, in order, or `None` where a block cannot
/// be addressed exactly.
///
/// - A body heading: `(ned/heading-with-text <body children> "<plain text>")`.
/// - A body block after a heading:
///   `(ned/nth-after (ned/heading-with-text ...) "<kind>" N)`, N counting only
///   blocks of this kind after the heading.
/// - A body block before any heading:
///   `(ned/nth-of-kind <body children> "<kind>" N)`.
/// - A slot element: `(ned/nth-of-kind <slot children> "<kind>" N)`.
///
/// Headings with equal plain text make each other ambiguous: the headings and
/// the blocks anchored to them get `None`. A link expression is never
/// addressed. `file` may be `""`; see [`with_file`].
pub fn block_addresses(
    file: &str,
    container: AddressContainer<'_>,
    elements: &[ContentElement],
) -> Vec<Option<String>> {
    let slot_name = match container {
        AddressContainer::Body => "body",
        AddressContainer::Slot(name) => name,
    };
    let children = format!("(ned/children (ned/slot {} {}))", doc_expr(file), clj_str(slot_name));
    let addressed = |el: &ContentElement| !matches!(el, ContentElement::LinkExpression { .. });

    let AddressContainer::Body = container else {
        let mut seen: HashMap<&str, usize> = HashMap::new();
        return elements
            .iter()
            .map(|el| {
                let kind = kind_name(el);
                let n = seen.entry(kind).or_insert(0);
                let idx = *n;
                *n += 1;
                addressed(el).then(|| format!("(ned/nth-of-kind {children} {} {idx})", clj_str(kind)))
            })
            .collect()
    };

    let heading_text = |el: &ContentElement| match el {
        ContentElement::Heading { content, .. } => Some(content.plain_text()),
        _ => None,
    };
    let mut heading_counts: HashMap<String, usize> = HashMap::new();
    for text in elements.iter().filter_map(heading_text) {
        *heading_counts.entry(text).or_insert(0) += 1;
    }

    // The heading blocks are currently anchored to, as its expression (None
    // when ambiguous), and how many blocks of each kind follow it.
    let mut anchor: Option<Option<String>> = None;
    let mut seen: HashMap<&str, usize> = HashMap::new();
    elements
        .iter()
        .map(|el| {
            if let Some(text) = heading_text(el) {
                let expr = (heading_counts[&text] == 1)
                    .then(|| format!("(ned/heading-with-text {children} {})", clj_str(&text)));
                anchor = Some(expr.clone());
                seen.clear();
                return expr;
            }
            let kind = kind_name(el);
            let n = seen.entry(kind).or_insert(0);
            let idx = *n;
            *n += 1;
            if !addressed(el) {
                return None;
            }
            match &anchor {
                None => Some(format!("(ned/nth-of-kind {children} {} {idx})", clj_str(kind))),
                Some(heading) => heading
                    .as_ref()
                    .map(|h| format!("(ned/nth-after {h} {} {idx})", clj_str(kind))),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema::HeadingLevel;

    fn h(source: &str) -> ContentElement {
        ContentElement::heading_from_markdown(HeadingLevel::new(2).unwrap(), source)
    }

    fn p(text: &str) -> ContentElement {
        ContentElement::paragraph_from_markdown(text)
    }

    fn code() -> ContentElement {
        ContentElement::CodeBlock { language: None, code: "x".to_string() }
    }

    const KIDS: &str = r#"(ned/children (ned/slot (ned/doc-by-path "content/p.md") "body"))"#;

    #[test]
    fn block_addresses_cases() {
        // Under a heading: the heading, then blocks counted per kind after it.
        let els = [h("Intro"), p("a"), code(), p("b"), h("Next"), p("c")];
        let got = block_addresses("content/p.md", AddressContainer::Body, &els);
        let intro = format!(r#"(ned/heading-with-text {KIDS} "Intro")"#);
        let next = format!(r#"(ned/heading-with-text {KIDS} "Next")"#);
        assert_eq!(
            got,
            vec![
                Some(intro.clone()),
                Some(format!(r#"(ned/nth-after {intro} "paragraph" 0)"#)),
                Some(format!(r#"(ned/nth-after {intro} "code-block" 0)"#)),
                Some(format!(r#"(ned/nth-after {intro} "paragraph" 1)"#)),
                Some(next.clone()),
                Some(format!(r#"(ned/nth-after {next} "paragraph" 0)"#)),
            ]
        );

        // Before any heading: counted from the start of the body.
        let els = [p("a"), code(), p("b"), h("H"), p("c")];
        let got = block_addresses("content/p.md", AddressContainer::Body, &els);
        assert_eq!(
            got[..3],
            [
                Some(format!(r#"(ned/nth-of-kind {KIDS} "paragraph" 0)"#)),
                Some(format!(r#"(ned/nth-of-kind {KIDS} "code-block" 0)"#)),
                Some(format!(r#"(ned/nth-of-kind {KIDS} "paragraph" 1)"#)),
            ]
        );

        // A slot: counted per kind within the slot.
        let slot = r#"(ned/children (ned/slot (ned/doc-by-path "content/p.md") "tags"))"#;
        let got = block_addresses(
            "content/p.md",
            AddressContainer::Slot("tags"),
            &[p("a"), p("b"), h("t")],
        );
        assert_eq!(
            got,
            vec![
                Some(format!(r#"(ned/nth-of-kind {slot} "paragraph" 0)"#)),
                Some(format!(r#"(ned/nth-of-kind {slot} "paragraph" 1)"#)),
                Some(format!(r#"(ned/nth-of-kind {slot} "heading" 0)"#)),
            ]
        );
    }

    #[test]
    fn duplicate_headings_are_not_addressable() {
        // Plain text decides: `*A*` and `A` are the same heading text.
        let els = [h("*A*"), p("under a"), h("A"), p("also under a"), h("B"), p("under b")];
        let got = block_addresses("f.md", AddressContainer::Body, &els);
        assert_eq!(got[..4], [None, None, None, None]);
        assert!(got[4].is_some() && got[5].is_some(), "{got:?}");
    }

    #[test]
    fn heading_text_is_plain_text_and_escaped() {
        let els = [h(r#"Say "hi" \ `x<y`"#)];
        let got = block_addresses("f.md", AddressContainer::Body, &els);
        let want = r#"(ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "f.md") "body")) "Say \"hi\" \\ x<y")"#;
        assert_eq!(got, vec![Some(want.to_string())]);
    }

    #[test]
    fn a_link_expression_is_not_addressed() {
        use crate::{LinkTarget, LinkText};
        let le = ContentElement::LinkExpression {
            text: LinkText::Empty,
            target: LinkTarget::PathRef("x".to_string()),
            slot: None,
        };
        let got = block_addresses("f.md", AddressContainer::Slot("s"), &[le]);
        assert_eq!(got, vec![None]);
    }

    #[test]
    fn with_file_fills_in_the_file_once() {
        let els = [h(r#"(ned/doc-by-path "")"#), p("x")];
        let got = block_addresses("", AddressContainer::Body, &els);
        let filled = with_file(got[0].as_deref().unwrap(), "c/x.md").unwrap();
        assert_eq!(
            Some(filled),
            block_addresses("c/x.md", AddressContainer::Body, &els)[0].clone()
        );
        assert_eq!(with_file("anything", ""), None);
    }
}
