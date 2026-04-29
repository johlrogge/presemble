//! Constraint extraction: converts grammar Slot constraints into HTML data attribute
//! name-value pairs for the structure-mode overlay.
//!
//! Each constraint on a slot becomes a `data-presemble-schema-constraints-<name>` attribute
//! on that slot's outer DOM element. The browser reads these via
//! `el.dataset['presembleSchemaConstraints<Name>']`.

use schema::{Constraint, ContentConstraint, CountRange, Orientation, Slot};

/// The attribute namespace prefix — all constraint attributes share this prefix.
pub const CONSTRAINT_ATTR_PREFIX: &str = "data-presemble-schema-constraints-";

/// Convert a single `CountRange` to the canonical string format.
///
/// | Variant                   | Output         |
/// |---------------------------|----------------|
/// | Exactly(1)                | exactly-once   |
/// | Exactly(n)                | n..n           |
/// | AtLeast(1)                | at-least-1     |
/// | AtLeast(n)                | n..*           |
/// | AtMost(n)                 | ..n            |
/// | Between { min, max }      | min..max       |
pub fn format_count_range(cr: &CountRange) -> String {
    match cr {
        CountRange::Exactly(1) => "exactly-once".to_string(),
        CountRange::Exactly(n) => format!("{n}..{n}"),
        CountRange::AtLeast(1) => "at-least-1".to_string(),
        CountRange::AtLeast(n) => format!("{n}..*"),
        CountRange::AtMost(n) => format!("..{n}"),
        CountRange::Between { min, max } => format!("{min}..{max}"),
    }
}

/// Convert a `ContentConstraint` to its string form.
pub fn format_content_constraint(cc: &ContentConstraint) -> String {
    match cc {
        ContentConstraint::Capitalized => "capitalized".to_string(),
    }
}

/// Convert an `Orientation` to its string form.
pub fn format_orientation(o: &Orientation) -> String {
    match o {
        Orientation::Landscape => "landscape".to_string(),
        Orientation::Portrait => "portrait".to_string(),
    }
}

/// Extract all constraints from a slot and return them as a list of
/// `(attribute-suffix, value-string)` pairs.
///
/// The caller adds `CONSTRAINT_ATTR_PREFIX` to the suffix to get the full attribute name.
///
/// Returns an empty vec if the slot has no constraints.
pub fn extract_slot_constraint_attrs(slot: &Slot) -> Vec<(String, String)> {
    let mut result = Vec::new();

    for constraint in &slot.constraints {
        match constraint {
            Constraint::Occurs(cr) => {
                result.push(("occurs".to_string(), format_count_range(cr)));
            }
            Constraint::Content(cc) => {
                result.push(("content".to_string(), format_content_constraint(cc)));
            }
            Constraint::Alt(alt_req) => {
                use schema::AltRequirement;
                let val = match alt_req {
                    AltRequirement::Required => "required",
                    AltRequirement::Optional => "optional",
                };
                result.push(("alt".to_string(), val.to_string()));
            }
            Constraint::Orientation(o) => {
                result.push(("orientation".to_string(), format_orientation(o)));
            }
            Constraint::TypeLink(name) => {
                result.push(("typelink".to_string(), name.clone()));
            }
        }
    }

    result
}

/// Build the full attribute name from a constraint suffix.
///
/// e.g. `"occurs"` -> `"data-presemble-schema-constraints-occurs"`
pub fn constraint_attr_name(suffix: &str) -> String {
    format!("{CONSTRAINT_ATTR_PREFIX}{suffix}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use schema::{
        AltRequirement, Constraint, ContentConstraint, CountRange, Element, HeadingLevel,
        HeadingLevelRange, Orientation, Slot, SlotName, Span,
    };

    fn make_slot(name: &str, constraints: Vec<Constraint>) -> Slot {
        Slot {
            name: SlotName::new(name),
            element: Element::Heading {
                level: HeadingLevelRange {
                    min: HeadingLevel::new(1).unwrap(),
                    max: HeadingLevel::new(1).unwrap(),
                },
            },
            constraints,
            hint_text: None,
            span: Span { start: 0, end: 0 },
        }
    }

    // -------------------------------------------------------------------------
    // T1: Occurs(Exactly(1)) → occurs=exactly-once
    // -------------------------------------------------------------------------
    #[test]
    fn constraint_extraction_emits_occurs() {
        let slot = make_slot("title", vec![Constraint::Occurs(CountRange::Exactly(1))]);
        let attrs = extract_slot_constraint_attrs(&slot);
        assert_eq!(attrs.len(), 1, "expected exactly one constraint attr");
        assert_eq!(attrs[0].0, "occurs");
        assert_eq!(attrs[0].1, "exactly-once");
    }

    // -------------------------------------------------------------------------
    // T2: TypeLink("author") → typelink=author
    // -------------------------------------------------------------------------
    #[test]
    fn constraint_extraction_emits_typelink() {
        let slot = make_slot("author", vec![Constraint::TypeLink("author".to_string())]);
        let attrs = extract_slot_constraint_attrs(&slot);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].0, "typelink");
        assert_eq!(attrs[0].1, "author");
    }

    // -------------------------------------------------------------------------
    // T3: Content(Capitalized) → content=capitalized
    // -------------------------------------------------------------------------
    #[test]
    fn constraint_extraction_emits_content() {
        let slot = make_slot(
            "title",
            vec![Constraint::Content(ContentConstraint::Capitalized)],
        );
        let attrs = extract_slot_constraint_attrs(&slot);
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].0, "content");
        assert_eq!(attrs[0].1, "capitalized");
    }

    // -------------------------------------------------------------------------
    // T4: Occurs(Between{1,3}) AND Content(Capitalized) → both attributes
    // -------------------------------------------------------------------------
    #[test]
    fn constraint_extraction_emits_multiple() {
        let slot = make_slot(
            "title",
            vec![
                Constraint::Occurs(CountRange::Between { min: 1, max: 3 }),
                Constraint::Content(ContentConstraint::Capitalized),
            ],
        );
        let attrs = extract_slot_constraint_attrs(&slot);
        assert_eq!(attrs.len(), 2, "expected two constraint attrs");

        let occurs = attrs.iter().find(|(k, _)| k == "occurs").expect("missing occurs");
        assert_eq!(occurs.1, "1..3");

        let content = attrs.iter().find(|(k, _)| k == "content").expect("missing content");
        assert_eq!(content.1, "capitalized");
    }

    // -------------------------------------------------------------------------
    // T5: Grammar with N slots → N constraint sets (one per slot)
    // -------------------------------------------------------------------------
    #[test]
    fn constraint_extraction_emits_for_all_grammar_slots() {
        use schema::Grammar;

        let grammar = Grammar {
            preamble: vec![
                make_slot("title", vec![Constraint::Occurs(CountRange::Exactly(1))]),
                make_slot("author", vec![Constraint::TypeLink("person".to_string())]),
                make_slot("cover", vec![
                    Constraint::Orientation(Orientation::Landscape),
                    Constraint::Alt(AltRequirement::Required),
                ]),
            ],
            body: None,
        };

        // Verify each slot produces a non-empty constraint set.
        let title_attrs = extract_slot_constraint_attrs(&grammar.preamble[0]);
        assert_eq!(title_attrs.len(), 1);
        assert_eq!(title_attrs[0].0, "occurs");

        let author_attrs = extract_slot_constraint_attrs(&grammar.preamble[1]);
        assert_eq!(author_attrs.len(), 1);
        assert_eq!(author_attrs[0].0, "typelink");

        let cover_attrs = extract_slot_constraint_attrs(&grammar.preamble[2]);
        assert_eq!(cover_attrs.len(), 2);
        assert!(cover_attrs.iter().any(|(k, v)| k == "orientation" && v == "landscape"));
        assert!(cover_attrs.iter().any(|(k, v)| k == "alt" && v == "required"));
    }

    // -------------------------------------------------------------------------
    // Additional: CountRange formatting edge cases
    // -------------------------------------------------------------------------
    #[test]
    fn format_count_range_exactly_once() {
        assert_eq!(format_count_range(&CountRange::Exactly(1)), "exactly-once");
    }

    #[test]
    fn format_count_range_exactly_n() {
        assert_eq!(format_count_range(&CountRange::Exactly(5)), "5..5");
    }

    #[test]
    fn format_count_range_at_least_1() {
        assert_eq!(format_count_range(&CountRange::AtLeast(1)), "at-least-1");
    }

    #[test]
    fn format_count_range_at_least_n() {
        assert_eq!(format_count_range(&CountRange::AtLeast(3)), "3..*");
    }

    #[test]
    fn format_count_range_at_most() {
        assert_eq!(format_count_range(&CountRange::AtMost(5)), "..5");
    }

    #[test]
    fn format_count_range_between() {
        assert_eq!(
            format_count_range(&CountRange::Between { min: 1, max: 3 }),
            "1..3"
        );
    }

    #[test]
    fn constraint_attr_name_adds_prefix() {
        assert_eq!(
            constraint_attr_name("occurs"),
            "data-presemble-schema-constraints-occurs"
        );
        assert_eq!(
            constraint_attr_name("typelink"),
            "data-presemble-schema-constraints-typelink"
        );
    }

    #[test]
    fn slot_with_no_constraints_returns_empty() {
        let slot = make_slot("title", vec![]);
        let attrs = extract_slot_constraint_attrs(&slot);
        assert!(attrs.is_empty());
    }
}
