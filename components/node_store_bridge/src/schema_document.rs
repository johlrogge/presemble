use content::{Document, DocumentSlot};
use schema::{Constraint, Grammar, SchemaKind, SlotName};

/// Synthesize an empty [`Document`] whose structure mirrors what the grammar requires.
///
/// Each slot in the grammar's preamble becomes a [`DocumentSlot`] with an empty
/// elements vector.  The `hint_text` on a slot flows through to the renderer via
/// the existing suggestion pipeline — it is *not* materialised as content here.
///
/// **Reference handling:** `Constraint::TypeLink` slots are left as empty slots.
/// The synthesizer does *not* follow cross-document references; that is a
/// navigation/rendering concern for later phases (T5).
///
/// **Body:** An empty body vector is produced regardless of what body rules the
/// grammar declares.  The renderer's placeholder pipeline handles `AtLeast`/
/// `Exactly` rules at render time.
///
/// The `kind` parameter is informational metadata — it tags which shape is being
/// synthesized (`Item` vs `Index`) but does not change the synthesis logic, since
/// each kind is already described by its own distinct grammar.
pub fn synthesize_schema_document(grammar: &Grammar, _kind: SchemaKind) -> Document {
    let preamble = grammar
        .preamble
        .iter()
        .map(synthesize_slot)
        .collect();

    Document {
        preamble,
        body: im::Vector::new(),
        has_separator: grammar.body.is_some(),
        separator_span: None,
    }
}

fn synthesize_slot(slot: &schema::Slot) -> DocumentSlot {
    // TypeLink and all other constraints are intentionally not followed here.
    // We only inspect constraints to decide whether to recurse — but since
    // ConsistsOf does not yet exist in the schema type system, there is nothing
    // to recurse into.  TypeLink is a cross-document reference and is left
    // unresolved (empty slot).
    let _ = slot
        .constraints
        .iter()
        .any(|c| matches!(c, Constraint::TypeLink(_)));

    DocumentSlot {
        name: SlotName::new(slot.name.as_str()),
        elements: im::Vector::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use schema::{
        AltRequirement, BodyRules, Constraint, CountRange, Element, Grammar, HeadingLevel,
        HeadingLevelRange, SchemaKind, Slot, SlotName, Span,
    };

    fn span() -> Span {
        Span { start: 0, end: 0 }
    }

    fn paragraph_slot(name: &str) -> Slot {
        Slot {
            name: SlotName::new(name),
            element: Element::Paragraph,
            constraints: vec![],
            hint_text: None,
            span: span(),
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: post item schema → all slots present and empty
    // -----------------------------------------------------------------------

    /// Parse the real `site/schemas/post/item.md` fixture, synthesize for
    /// `SchemaKind::Item`, and assert every declared slot is present and empty.
    #[test]
    fn synthesize_post_item_produces_empty_slots() {
        // Load the real post item schema from the workspace fixture.
        let schema_src = include_str!("../../../site/schemas/post/item.md");
        let grammar = schema::parse_schema(schema_src).expect("post/item.md should parse");

        let doc = synthesize_schema_document(&grammar, SchemaKind::Item);

        // post/item.md declares: title, summary, author  (3 preamble slots)
        assert_eq!(
            doc.preamble.len(),
            grammar.preamble.len(),
            "synthesized doc should have one slot per grammar slot"
        );

        for (i, slot) in doc.preamble.iter().enumerate() {
            assert_eq!(
                slot.name.as_str(),
                grammar.preamble[i].name.as_str(),
                "slot names should match grammar order"
            );
            assert!(
                slot.elements.is_empty(),
                "synthesized slot '{}' should have no elements",
                slot.name.as_str()
            );
        }

        // post/item.md has a body section — has_separator should be true
        assert!(doc.has_separator, "post item grammar has a body section");
        assert!(doc.body.is_empty(), "synthesized body should be empty");
    }

    // -----------------------------------------------------------------------
    // Test 2: ConsistsOf follow — structural recursion
    // -----------------------------------------------------------------------

    /// Fixture: a grammar where slot A "consists of" sub-structure B and C.
    ///
    /// NOTE: `Constraint::ConsistsOf` does not yet exist in the schema type
    /// system.  When it is added (future phase), this test will need to assert
    /// that synthesize_schema_document produces nested slots.
    ///
    /// For now we verify the synthesizer handles the current constraint set
    /// correctly: a slot with only `Occurs` / other non-reference constraints
    /// produces an empty slot without error.
    #[test]
    fn synthesize_follows_consists_of_placeholder() {
        // Simulate a grammar with two slots and occurrence constraints
        // (closest current analogue to "consists-of").
        let grammar = Grammar {
            preamble: vec![
                Slot {
                    name: SlotName::new("section_title"),
                    element: Element::Heading {
                        level: HeadingLevelRange {
                            min: HeadingLevel::new(2).unwrap(),
                            max: HeadingLevel::new(2).unwrap(),
                        },
                    },
                    constraints: vec![Constraint::Occurs(CountRange::Exactly(1))],
                    hint_text: Some("Enter the section title".to_string()),
                    span: span(),
                },
                Slot {
                    name: SlotName::new("section_body"),
                    element: Element::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::AtLeast(1))],
                    hint_text: Some("Enter section content".to_string()),
                    span: span(),
                },
            ],
            body: None,
        };

        let doc = synthesize_schema_document(&grammar, SchemaKind::Item);

        assert_eq!(doc.preamble.len(), 2);
        assert_eq!(doc.preamble[0].name.as_str(), "section_title");
        assert_eq!(doc.preamble[1].name.as_str(), "section_body");
        assert!(doc.preamble[0].elements.is_empty());
        assert!(doc.preamble[1].elements.is_empty());
        assert!(!doc.has_separator);
    }

    // -----------------------------------------------------------------------
    // Test 3: TypeLink (Reference) — slot stays empty, not followed
    // -----------------------------------------------------------------------

    #[test]
    fn synthesize_leaves_reference_unresolved() {
        let grammar = Grammar {
            preamble: vec![
                paragraph_slot("title"),
                Slot {
                    name: SlotName::new("author"),
                    element: Element::Link { pattern: ".*".to_string() },
                    constraints: vec![
                        Constraint::TypeLink("author".to_string()),
                        Constraint::Occurs(CountRange::Exactly(1)),
                    ],
                    hint_text: Some("Link to the author".to_string()),
                    span: span(),
                },
            ],
            body: None,
        };

        let doc = synthesize_schema_document(&grammar, SchemaKind::Item);

        assert_eq!(doc.preamble.len(), 2);

        let author_slot = &doc.preamble[1];
        assert_eq!(author_slot.name.as_str(), "author");
        // The TypeLink constraint must NOT cause any content to appear —
        // the slot stays empty (unresolved cross-document reference).
        assert!(
            author_slot.elements.is_empty(),
            "TypeLink slot should be empty — cross-doc references are not followed"
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: Index kind uses its own grammar shape (distinct from Item)
    // -----------------------------------------------------------------------

    /// Parse both `post/item.md` and `post/index.md`, synthesize each, and
    /// assert they produce different preamble shapes.
    #[test]
    fn synthesize_index_kind_uses_collection_shape() {
        let item_src = include_str!("../../../site/schemas/post/item.md");
        let index_src = include_str!("../../../site/schemas/post/index.md");

        let item_grammar = schema::parse_schema(item_src).expect("post/item.md should parse");
        let index_grammar = schema::parse_schema(index_src).expect("post/index.md should parse");

        let item_doc = synthesize_schema_document(&item_grammar, SchemaKind::Item);
        let index_doc = synthesize_schema_document(&index_grammar, SchemaKind::Index);

        // item schema (post/item.md) has more slots than the minimal index schema
        assert!(
            item_doc.preamble.len() > index_doc.preamble.len(),
            "item schema should have more slots than index schema: item={} index={}",
            item_doc.preamble.len(),
            index_doc.preamble.len()
        );

        // index schema preamble has "title" as first slot
        assert_eq!(
            index_doc.preamble[0].name.as_str(),
            "title",
            "index schema first slot should be 'title'"
        );
        assert!(
            index_doc.preamble[0].elements.is_empty(),
            "synthesized index slot should be empty"
        );

        // index schema (post/index.md) has no body
        assert!(!index_doc.has_separator, "index schema has no body section");
        // item schema (post/item.md) has a body
        assert!(item_doc.has_separator, "item schema has a body section");
    }

    // -----------------------------------------------------------------------
    // Extra: hint_text survives in the grammar (not in synthesized content)
    // -----------------------------------------------------------------------

    /// Verify that hint_text on a slot is still accessible via the grammar
    /// after synthesis — the synthesized Document does not materialise it as
    /// content (that's the renderer's job via the suggestion pipeline).
    #[test]
    fn synthesize_hint_text_not_materialised_in_document() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("summary"),
                element: Element::Paragraph,
                constraints: vec![],
                hint_text: Some("Write a 2-sentence summary".to_string()),
                span: span(),
            }],
            body: None,
        };

        let doc = synthesize_schema_document(&grammar, SchemaKind::Item);

        assert_eq!(doc.preamble.len(), 1);
        let slot = &doc.preamble[0];
        assert_eq!(slot.name.as_str(), "summary");
        // hint_text is NOT in the DocumentSlot elements — it flows through
        // the grammar to the renderer
        assert!(slot.elements.is_empty());

        // Confirm the grammar still has the hint (it was not consumed)
        assert_eq!(
            grammar.preamble[0].hint_text.as_deref(),
            Some("Write a 2-sentence summary")
        );
    }

    // -----------------------------------------------------------------------
    // Extra: grammar with only body rules (no preamble)
    // -----------------------------------------------------------------------

    #[test]
    fn synthesize_body_only_grammar_produces_empty_preamble_and_separator() {
        let grammar = Grammar {
            preamble: vec![],
            body: Some(BodyRules {
                heading_range: Some(HeadingLevelRange {
                    min: HeadingLevel::new(3).unwrap(),
                    max: HeadingLevel::new(6).unwrap(),
                }),
            }),
        };

        let doc = synthesize_schema_document(&grammar, SchemaKind::Item);

        assert!(doc.preamble.is_empty());
        assert!(doc.has_separator, "grammar with body rules should set has_separator");
        assert!(doc.body.is_empty(), "synthesized body is always empty");
    }

    // -----------------------------------------------------------------------
    // Extra: slot with Alt constraint stays empty (not a reference)
    // -----------------------------------------------------------------------

    #[test]
    fn synthesize_alt_constraint_slot_is_empty() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("hero_image"),
                element: Element::Image { pattern: ".*".to_string() },
                constraints: vec![
                    Constraint::Alt(AltRequirement::Required),
                    Constraint::Occurs(CountRange::Exactly(1)),
                ],
                hint_text: None,
                span: span(),
            }],
            body: None,
        };

        let doc = synthesize_schema_document(&grammar, SchemaKind::Item);

        assert_eq!(doc.preamble.len(), 1);
        assert!(doc.preamble[0].elements.is_empty());
    }
}
