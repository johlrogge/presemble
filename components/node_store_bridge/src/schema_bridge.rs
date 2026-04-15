use node_store::{Edge, Node, NodeId, NodeStore};
use schema::{
    AltRequirement, BodyRules, Constraint, ContentConstraint, CountRange, Element, Grammar,
    HeadingLevel, HeadingLevelRange, Orientation, Slot, SlotName, Span,
};

// ---------------------------------------------------------------------------
// grammar_to_store
// ---------------------------------------------------------------------------

/// Converts a `Grammar` into a NodeStore subtree and returns the root NodeId.
pub fn grammar_to_store(grammar: &Grammar, store: &mut NodeStore) -> NodeId {
    let grammar_name = store.intern("grammar");
    let root = store.add_node(Node::Element(grammar_name));

    // preamble
    let preamble_name = store.intern("preamble");
    let preamble = store.add_node(Node::Element(preamble_name));
    store.add_edge(root, Edge::Child(preamble));

    for slot in &grammar.preamble {
        let slot_id = slot_to_store(slot, store);
        store.add_edge(preamble, Edge::Child(slot_id));
    }

    // body-rules (optional)
    if let Some(body) = &grammar.body {
        let body_id = body_rules_to_store(body, store);
        store.add_edge(root, Edge::Child(body_id));
    }

    root
}

fn slot_to_store(slot: &Slot, store: &mut NodeStore) -> NodeId {
    let slot_name = store.intern("slot");
    let id = store.add_node(Node::Element(slot_name));

    // name attribute
    let name_attr = store.intern("name");
    let name_val = store.add_node(Node::Text(slot.name.as_str().to_string()));
    store.add_edge(id, Edge::Attribute { name: name_attr, value: name_val });

    // element child
    let element_id = element_to_store(&slot.element, store);
    store.add_edge(id, Edge::Child(element_id));

    // constraint children
    for constraint in &slot.constraints {
        let c_id = constraint_to_store(constraint, store);
        store.add_edge(id, Edge::Child(c_id));
    }

    // hint attribute (optional)
    if let Some(hint) = &slot.hint_text {
        let hint_attr = store.intern("hint");
        let hint_val = store.add_node(Node::Text(hint.clone()));
        store.add_edge(id, Edge::Attribute { name: hint_attr, value: hint_val });
    }

    id
}

fn element_to_store(element: &Element, store: &mut NodeStore) -> NodeId {
    match element {
        Element::Heading { level } => {
            let name = store.intern("heading");
            let id = store.add_node(Node::Element(name));

            let min_attr = store.intern("min-level");
            let min_val = store.add_node(Node::Integer(level.min.value() as i64));
            store.add_edge(id, Edge::Attribute { name: min_attr, value: min_val });

            let max_attr = store.intern("max-level");
            let max_val = store.add_node(Node::Integer(level.max.value() as i64));
            store.add_edge(id, Edge::Attribute { name: max_attr, value: max_val });

            id
        }
        Element::Paragraph => {
            let name = store.intern("paragraph");
            store.add_node(Node::Element(name))
        }
        Element::Link { pattern } => {
            let name = store.intern("link");
            let id = store.add_node(Node::Element(name));

            let pattern_attr = store.intern("pattern");
            let pattern_val = store.add_node(Node::Text(pattern.clone()));
            store.add_edge(id, Edge::Attribute { name: pattern_attr, value: pattern_val });

            id
        }
        Element::Image { pattern } => {
            let name = store.intern("image");
            let id = store.add_node(Node::Element(name));

            let pattern_attr = store.intern("pattern");
            let pattern_val = store.add_node(Node::Text(pattern.clone()));
            store.add_edge(id, Edge::Attribute { name: pattern_attr, value: pattern_val });

            id
        }
        Element::List => {
            let name = store.intern("list");
            store.add_node(Node::Element(name))
        }
    }
}

fn constraint_to_store(constraint: &Constraint, store: &mut NodeStore) -> NodeId {
    match constraint {
        Constraint::Occurs(count_range) => {
            let name = store.intern("occurs");
            let id = store.add_node(Node::Element(name));

            let kind_attr = store.intern("kind");

            match count_range {
                CountRange::Exactly(n) => {
                    let kind_val = store.add_node(Node::Text("exactly".to_string()));
                    store.add_edge(id, Edge::Attribute { name: kind_attr, value: kind_val });
                    let value_attr = store.intern("value");
                    let value_val = store.add_node(Node::Integer(*n as i64));
                    store.add_edge(id, Edge::Attribute { name: value_attr, value: value_val });
                }
                CountRange::AtLeast(n) => {
                    let kind_val = store.add_node(Node::Text("at-least".to_string()));
                    store.add_edge(id, Edge::Attribute { name: kind_attr, value: kind_val });
                    let value_attr = store.intern("value");
                    let value_val = store.add_node(Node::Integer(*n as i64));
                    store.add_edge(id, Edge::Attribute { name: value_attr, value: value_val });
                }
                CountRange::AtMost(n) => {
                    let kind_val = store.add_node(Node::Text("at-most".to_string()));
                    store.add_edge(id, Edge::Attribute { name: kind_attr, value: kind_val });
                    let value_attr = store.intern("value");
                    let value_val = store.add_node(Node::Integer(*n as i64));
                    store.add_edge(id, Edge::Attribute { name: value_attr, value: value_val });
                }
                CountRange::Between { min, max } => {
                    let kind_val = store.add_node(Node::Text("between".to_string()));
                    store.add_edge(id, Edge::Attribute { name: kind_attr, value: kind_val });
                    let min_attr = store.intern("min");
                    let min_val = store.add_node(Node::Integer(*min as i64));
                    store.add_edge(id, Edge::Attribute { name: min_attr, value: min_val });
                    let max_attr = store.intern("max");
                    let max_val = store.add_node(Node::Integer(*max as i64));
                    store.add_edge(id, Edge::Attribute { name: max_attr, value: max_val });
                }
            }

            id
        }
        Constraint::Content(ContentConstraint::Capitalized) => {
            let name = store.intern("content-constraint");
            let id = store.add_node(Node::Element(name));

            let kind_attr = store.intern("kind");
            let kind_val = store.add_node(Node::Text("capitalized".to_string()));
            store.add_edge(id, Edge::Attribute { name: kind_attr, value: kind_val });

            id
        }
        Constraint::Alt(alt) => {
            let name = store.intern("alt-constraint");
            let id = store.add_node(Node::Element(name));

            let required_attr = store.intern("required");
            let required_val = store.add_node(Node::Boolean(matches!(alt, AltRequirement::Required)));
            store.add_edge(id, Edge::Attribute { name: required_attr, value: required_val });

            id
        }
        Constraint::Orientation(orientation) => {
            let name = store.intern("orientation-constraint");
            let id = store.add_node(Node::Element(name));

            let value_attr = store.intern("value");
            let orientation_str = match orientation {
                Orientation::Landscape => "landscape",
                Orientation::Portrait => "portrait",
            };
            let value_val = store.add_node(Node::Text(orientation_str.to_string()));
            store.add_edge(id, Edge::Attribute { name: value_attr, value: value_val });

            id
        }
        Constraint::TypeLink(schema_name) => {
            let name = store.intern("type-link");
            let id = store.add_node(Node::Element(name));

            let schema_attr = store.intern("schema");
            let schema_val = store.add_node(Node::Text(schema_name.clone()));
            store.add_edge(id, Edge::Attribute { name: schema_attr, value: schema_val });

            id
        }
    }
}

fn body_rules_to_store(body: &BodyRules, store: &mut NodeStore) -> NodeId {
    let name = store.intern("body-rules");
    let id = store.add_node(Node::Element(name));

    if let Some(range) = &body.heading_range {
        let heading_range_name = store.intern("heading-range");
        let range_id = store.add_node(Node::Element(heading_range_name));

        let min_attr = store.intern("min");
        let min_val = store.add_node(Node::Integer(range.min.value() as i64));
        store.add_edge(range_id, Edge::Attribute { name: min_attr, value: min_val });

        let max_attr = store.intern("max");
        let max_val = store.add_node(Node::Integer(range.max.value() as i64));
        store.add_edge(range_id, Edge::Attribute { name: max_attr, value: max_val });

        store.add_edge(id, Edge::Child(range_id));
    }

    id
}

// ---------------------------------------------------------------------------
// store_to_grammar
// ---------------------------------------------------------------------------

/// Reconstructs a `Grammar` from a NodeStore subtree rooted at `root`.
///
/// Spans are set to `Span { start: 0, end: 0 }` since byte positions do not
/// survive a round-trip through the NodeStore.
pub fn store_to_grammar(store: &NodeStore, root: NodeId) -> Grammar {
    let children = store.children(root);

    let mut preamble = Vec::new();
    let mut body = None;

    for child in children {
        if let Some(Node::Element(name)) = store.get(child) {
            let tag = store.resolve_name(*name);
            match tag {
                "preamble" => {
                    for slot_id in store.children(child) {
                        preamble.push(store_to_slot(store, slot_id));
                    }
                }
                "body-rules" => {
                    body = Some(store_to_body_rules(store, child));
                }
                _ => {}
            }
        }
    }

    Grammar { preamble, body }
}

fn store_to_slot(store: &NodeStore, id: NodeId) -> Slot {
    let name_str = find_attr_text(store, id, "name").unwrap_or_default();
    let hint_text = find_attr_text(store, id, "hint");

    let mut element = Element::Paragraph;
    let mut constraints = Vec::new();

    for child in store.children(id) {
        if let Some(Node::Element(tag_name)) = store.get(child) {
            let tag = store.resolve_name(*tag_name);
            match tag {
                "heading" => element = store_to_heading_element(store, child),
                "paragraph" => element = Element::Paragraph,
                "link" => {
                    let pattern = find_attr_text(store, child, "pattern").unwrap_or_default();
                    element = Element::Link { pattern };
                }
                "image" => {
                    let pattern = find_attr_text(store, child, "pattern").unwrap_or_default();
                    element = Element::Image { pattern };
                }
                "list" => element = Element::List,
                "occurs" => constraints.push(store_to_occurs_constraint(store, child)),
                "content-constraint" => {
                    constraints.push(Constraint::Content(ContentConstraint::Capitalized));
                }
                "alt-constraint" => {
                    let required = find_attr_bool(store, child, "required").unwrap_or(false);
                    let alt = if required {
                        AltRequirement::Required
                    } else {
                        AltRequirement::Optional
                    };
                    constraints.push(Constraint::Alt(alt));
                }
                "orientation-constraint" => {
                    let val = find_attr_text(store, child, "value").unwrap_or_default();
                    let orientation = if val == "landscape" {
                        Orientation::Landscape
                    } else {
                        Orientation::Portrait
                    };
                    constraints.push(Constraint::Orientation(orientation));
                }
                "type-link" => {
                    let schema_name = find_attr_text(store, child, "schema").unwrap_or_default();
                    constraints.push(Constraint::TypeLink(schema_name));
                }
                _ => {}
            }
        }
    }

    Slot {
        name: SlotName::new(name_str),
        element,
        constraints,
        hint_text,
        span: Span { start: 0, end: 0 },
    }
}

fn store_to_heading_element(store: &NodeStore, id: NodeId) -> Element {
    let min_val = find_attr_int(store, id, "min-level").unwrap_or(1) as u8;
    let max_val = find_attr_int(store, id, "max-level").unwrap_or(6) as u8;

    let min = HeadingLevel::new(min_val).unwrap_or_else(|| HeadingLevel::new(1).unwrap());
    let max = HeadingLevel::new(max_val).unwrap_or_else(|| HeadingLevel::new(6).unwrap());

    Element::Heading {
        level: HeadingLevelRange { min, max },
    }
}

fn store_to_occurs_constraint(store: &NodeStore, id: NodeId) -> Constraint {
    let kind = find_attr_text(store, id, "kind").unwrap_or_default();

    let count_range = match kind.as_str() {
        "exactly" => {
            let n = find_attr_int(store, id, "value").unwrap_or(1) as usize;
            CountRange::Exactly(n)
        }
        "at-least" => {
            let n = find_attr_int(store, id, "value").unwrap_or(1) as usize;
            CountRange::AtLeast(n)
        }
        "at-most" => {
            let n = find_attr_int(store, id, "value").unwrap_or(1) as usize;
            CountRange::AtMost(n)
        }
        "between" => {
            let min = find_attr_int(store, id, "min").unwrap_or(1) as usize;
            let max = find_attr_int(store, id, "max").unwrap_or(1) as usize;
            CountRange::Between { min, max }
        }
        _ => CountRange::Exactly(1),
    };

    Constraint::Occurs(count_range)
}

fn store_to_body_rules(store: &NodeStore, id: NodeId) -> BodyRules {
    let mut heading_range = None;

    for child in store.children(id) {
        if let Some(Node::Element(tag_name)) = store.get(child) {
            let tag = store.resolve_name(*tag_name);
            if tag == "heading-range" {
                let min_val = find_attr_int(store, child, "min").unwrap_or(1) as u8;
                let max_val = find_attr_int(store, child, "max").unwrap_or(6) as u8;
                let min = HeadingLevel::new(min_val).unwrap_or_else(|| HeadingLevel::new(1).unwrap());
                let max = HeadingLevel::new(max_val).unwrap_or_else(|| HeadingLevel::new(6).unwrap());
                heading_range = Some(HeadingLevelRange { min, max });
            }
        }
    }

    BodyRules { heading_range }
}

// ---------------------------------------------------------------------------
// Attribute lookup helpers
// ---------------------------------------------------------------------------

fn find_attr_text(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<String> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Text(s)) = store.get(value_id)
        {
            return Some(s.clone());
        }
    }
    None
}

fn find_attr_int(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<i64> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Integer(n)) = store.get(value_id)
        {
            return Some(*n);
        }
    }
    None
}

fn find_attr_bool(store: &NodeStore, node: NodeId, attr_name: &str) -> Option<bool> {
    for (name, value_id) in store.attributes(node) {
        if store.resolve_name(name) == attr_name
            && let Some(Node::Boolean(b)) = store.get(value_id)
        {
            return Some(*b);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> NodeStore {
        NodeStore::new()
    }

    // Helper: assert element tag matches
    fn assert_element_tag(element: &Element, expected: &str) {
        let tag = match element {
            Element::Heading { .. } => "heading",
            Element::Paragraph => "paragraph",
            Element::Link { .. } => "link",
            Element::Image { .. } => "image",
            Element::List => "list",
        };
        assert_eq!(tag, expected, "Expected element tag '{expected}' but got '{tag}'");
    }

    #[test]
    fn round_trip_simple_paragraph_slot() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("title"),
                element: Element::Paragraph,
                constraints: vec![],
                hint_text: None,
                span: Span { start: 0, end: 0 },
            }],
            body: None,
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert_eq!(result.preamble.len(), 1);
        assert_eq!(result.preamble[0].name.as_str(), "title");
        assert_element_tag(&result.preamble[0].element, "paragraph");
        assert!(result.preamble[0].constraints.is_empty());
        assert!(result.preamble[0].hint_text.is_none());
        assert!(result.body.is_none());
    }

    #[test]
    fn round_trip_heading_slot_with_range() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("headline"),
                element: Element::Heading {
                    level: HeadingLevelRange {
                        min: HeadingLevel::new(2).unwrap(),
                        max: HeadingLevel::new(4).unwrap(),
                    },
                },
                constraints: vec![],
                hint_text: None,
                span: Span { start: 0, end: 0 },
            }],
            body: None,
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert_eq!(result.preamble.len(), 1);
        assert_eq!(result.preamble[0].name.as_str(), "headline");
        if let Element::Heading { level } = &result.preamble[0].element {
            assert_eq!(level.min.value(), 2);
            assert_eq!(level.max.value(), 4);
        } else {
            panic!("Expected Heading element");
        }
    }

    #[test]
    fn round_trip_multiple_constraints() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("body"),
                element: Element::Paragraph,
                constraints: vec![
                    Constraint::Occurs(CountRange::Between { min: 1, max: 5 }),
                    Constraint::Content(ContentConstraint::Capitalized),
                    Constraint::Alt(AltRequirement::Required),
                ],
                hint_text: None,
                span: Span { start: 0, end: 0 },
            }],
            body: None,
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert_eq!(result.preamble.len(), 1);
        let slot = &result.preamble[0];
        assert_eq!(slot.constraints.len(), 3);

        // Check Occurs(Between)
        if let Constraint::Occurs(CountRange::Between { min, max }) = &slot.constraints[0] {
            assert_eq!(*min, 1);
            assert_eq!(*max, 5);
        } else {
            panic!("Expected Occurs(Between) constraint");
        }

        // Check Content(Capitalized)
        assert!(matches!(slot.constraints[1], Constraint::Content(ContentConstraint::Capitalized)));

        // Check Alt(Required)
        assert!(matches!(slot.constraints[2], Constraint::Alt(AltRequirement::Required)));
    }

    #[test]
    fn round_trip_with_body_rules() {
        let grammar = Grammar {
            preamble: vec![],
            body: Some(BodyRules {
                heading_range: Some(HeadingLevelRange {
                    min: HeadingLevel::new(2).unwrap(),
                    max: HeadingLevel::new(3).unwrap(),
                }),
            }),
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert!(result.body.is_some());
        let body = result.body.unwrap();
        assert!(body.heading_range.is_some());
        let range = body.heading_range.unwrap();
        assert_eq!(range.min.value(), 2);
        assert_eq!(range.max.value(), 3);
    }

    #[test]
    fn round_trip_type_link_constraint() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("related"),
                element: Element::Link { pattern: ".*".to_string() },
                constraints: vec![Constraint::TypeLink("post".to_string())],
                hint_text: None,
                span: Span { start: 0, end: 0 },
            }],
            body: None,
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert_eq!(result.preamble.len(), 1);
        let slot = &result.preamble[0];
        assert_eq!(slot.name.as_str(), "related");

        if let Element::Link { pattern } = &slot.element {
            assert_eq!(pattern, ".*");
        } else {
            panic!("Expected Link element");
        }

        assert_eq!(slot.constraints.len(), 1);
        if let Constraint::TypeLink(schema_name) = &slot.constraints[0] {
            assert_eq!(schema_name, "post");
        } else {
            panic!("Expected TypeLink constraint");
        }
    }

    #[test]
    fn round_trip_with_hint_text() {
        let grammar = Grammar {
            preamble: vec![Slot {
                name: SlotName::new("caption"),
                element: Element::Paragraph,
                constraints: vec![],
                hint_text: Some("Write a brief caption here".to_string()),
                span: Span { start: 0, end: 0 },
            }],
            body: None,
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert_eq!(result.preamble.len(), 1);
        let slot = &result.preamble[0];
        assert_eq!(slot.hint_text.as_deref(), Some("Write a brief caption here"));
    }

    #[test]
    fn round_trip_all_occurs_variants() {
        let grammar = Grammar {
            preamble: vec![
                Slot {
                    name: SlotName::new("s1"),
                    element: Element::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::Exactly(3))],
                    hint_text: None,
                    span: Span { start: 0, end: 0 },
                },
                Slot {
                    name: SlotName::new("s2"),
                    element: Element::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::AtLeast(2))],
                    hint_text: None,
                    span: Span { start: 0, end: 0 },
                },
                Slot {
                    name: SlotName::new("s3"),
                    element: Element::Paragraph,
                    constraints: vec![Constraint::Occurs(CountRange::AtMost(4))],
                    hint_text: None,
                    span: Span { start: 0, end: 0 },
                },
            ],
            body: None,
        };

        let mut store = make_store();
        let root = grammar_to_store(&grammar, &mut store);
        let result = store_to_grammar(&store, root);

        assert_eq!(result.preamble.len(), 3);

        if let Constraint::Occurs(CountRange::Exactly(n)) = &result.preamble[0].constraints[0] {
            assert_eq!(*n, 3);
        } else {
            panic!("Expected Exactly(3)");
        }

        if let Constraint::Occurs(CountRange::AtLeast(n)) = &result.preamble[1].constraints[0] {
            assert_eq!(*n, 2);
        } else {
            panic!("Expected AtLeast(2)");
        }

        if let Constraint::Occurs(CountRange::AtMost(n)) = &result.preamble[2].constraints[0] {
            assert_eq!(*n, 4);
        } else {
            panic!("Expected AtMost(4)");
        }
    }
}
