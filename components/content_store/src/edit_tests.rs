//! Tests for `edit`: resolve, prepare and commit.
//!
//! The fixture is built by hand, in the encoding `document_to_store` writes:
//! `document > preamble > slot > block` and `document > body > block`.

use content::{ContentElement, RoundTripError};
use editorial_types::StaleCause;
use ned::{NodeTree, Selection};
use node_store::{Edge, Node, NodeId, NodeStore};
use schema::HeadingLevel;

use crate::access::{add_child_element, add_text_attr, find_child_by_name, make_element};
use crate::block::{BlockDecodeError, add_trivia, block_from_node, block_to_store};
use crate::edit::{
    AddressError, Committed, EditError, Executed, NedMutation, Plan, PlanCause, PlanError, PlannedMutation,
    PreparedEdit, Target, TargetKind, commit, execute, prepare, resolve,
};

fn heading(level: u8, text: &str) -> ContentElement {
    ContentElement::heading_from_markdown(HeadingLevel::new(level).unwrap(), text)
}

fn para(text: &str) -> ContentElement {
    ContentElement::paragraph_from_markdown(text)
}

fn para_tree(text: &str) -> NodeTree {
    NodeTree::element("paragraph").with_child(NodeTree::text(text))
}

fn add_block(store: &mut NodeStore, parent: NodeId, el: &ContentElement) -> NodeId {
    let id = block_to_store(el, store);
    store.add_edge(parent, Edge::Child(id));
    id
}

fn add_slot(store: &mut NodeStore, preamble: NodeId, name: &str, blocks: &[ContentElement]) {
    let slot = add_child_element(store, preamble, "slot");
    add_text_attr(store, slot, "name", name);
    for block in blocks {
        add_block(store, slot, block);
    }
}

/// One live document:
/// - slot "title": heading "Title"
/// - slot "summary": two paragraphs "One", "Two"
/// - body: paragraph "a", paragraph "b"
fn build_document(store: &mut NodeStore) -> NodeId {
    let doc = make_element(store, "document");
    let preamble = add_child_element(store, doc, "preamble");
    add_slot(store, preamble, "title", &[heading(1, "Title")]);
    add_slot(store, preamble, "summary", &[para("One"), para("Two")]);
    let body = add_child_element(store, doc, "body");
    add_block(store, body, &para("a"));
    add_block(store, body, &para("b"));
    doc
}

struct Fixture {
    store: NodeStore,
    doc: NodeId,
}

fn fixture() -> Fixture {
    let mut store = NodeStore::new();
    let doc = build_document(&mut store);
    Fixture { store, doc }
}

impl Fixture {
    fn slot(&self, name: &str) -> NodeId {
        let preamble = find_child_by_name(&self.store, self.doc, "preamble").unwrap();
        self.store
            .children(preamble)
            .into_iter()
            .find(|s| crate::access::find_attr_text(&self.store, *s, "name").as_deref() == Some(name))
            .unwrap()
    }
    fn body(&self) -> NodeId {
        find_child_by_name(&self.store, self.doc, "body").unwrap()
    }
    fn body_blocks(&self) -> Vec<NodeId> {
        self.store.children(self.body())
    }
    fn text_of(&self, block: NodeId) -> NodeId {
        self.store.children(block)[0]
    }
    fn body_texts(&self) -> Vec<String> {
        self.body_blocks()
            .into_iter()
            .map(|b| match self.store.get(self.text_of(b)) {
                Some(Node::Text(s)) => s.clone(),
                other => panic!("expected text, got {other:?}"),
            })
            .collect()
    }
    fn resolve(&self, ids: &[NodeId], kind: TargetKind) -> Result<Target, AddressError> {
        resolve(&self.store, &Selection::from_ids(ids.iter().copied()), kind)
    }
    fn prepare(&self, id: NodeId, m: &NedMutation) -> Result<PreparedEdit, EditError> {
        let target = self.resolve(&[id], m.target_kind()).expect("resolves");
        prepare(&self.store, &target, m)
    }
    fn run(&mut self, id: NodeId, m: &NedMutation) -> Committed {
        let edit = self.prepare(id, m).expect("valid");
        commit(&mut self.store, vec![edit])
    }
}

// ── resolve ─────────────────────────────────────────────────────────────────

#[test]
fn text_of_single_text_heading_resolves() {
    let f = fixture();
    let heading = f.store.children(f.slot("title"))[0];
    let text = f.text_of(heading);
    let got = f.resolve(&[text], TargetKind::Text).unwrap();
    assert_eq!(got, Target::Text { text, block: heading, document: f.doc });
}

#[test]
fn empty_selection_is_unresolved() {
    let f = fixture();
    assert_eq!(f.resolve(&[], TargetKind::Text), Err(AddressError::Unresolved));
}

#[test]
fn two_nodes_are_ambiguous() {
    let f = fixture();
    let texts: Vec<NodeId> =
        f.store.children(f.slot("summary")).into_iter().map(|b| f.text_of(b)).collect();
    assert_eq!(f.resolve(&texts, TargetKind::Text), Err(AddressError::Ambiguous { matches: 2 }));
}

#[test]
fn missing_node_is_unresolved() {
    let f = fixture();
    assert_eq!(f.resolve(&[NodeId(999_999)], TargetKind::Block), Err(AddressError::Unresolved));
}

#[test]
fn block_where_text_expected_is_wrong_kind() {
    let f = fixture();
    let heading = f.store.children(f.slot("title"))[0];
    assert_eq!(
        f.resolve(&[heading], TargetKind::Text),
        Err(AddressError::WrongKind { expected: TargetKind::Text, found: "heading".to_string() })
    );
}

#[test]
fn slot_where_block_expected_is_wrong_kind() {
    let f = fixture();
    assert_eq!(
        f.resolve(&[f.slot("title")], TargetKind::Block),
        Err(AddressError::WrongKind { expected: TargetKind::Block, found: "slot".to_string() })
    );
}

#[test]
fn body_and_slot_resolve_as_container_but_block_does_not() {
    let f = fixture();
    let body = f.body();
    assert_eq!(
        f.resolve(&[body], TargetKind::Container),
        Ok(Target::Container { container: body, document: f.doc })
    );
    let slot = f.slot("summary");
    assert_eq!(
        f.resolve(&[slot], TargetKind::Container),
        Ok(Target::Container { container: slot, document: f.doc })
    );
    let block = f.body_blocks()[0];
    assert!(matches!(
        f.resolve(&[block], TargetKind::Container),
        Err(AddressError::WrongKind { expected: TargetKind::Container, .. })
    ));
}

#[test]
fn block_resolves_with_container_and_document() {
    let f = fixture();
    let block = f.body_blocks()[1];
    assert_eq!(
        f.resolve(&[block], TargetKind::Block),
        Ok(Target::Block { block, container: f.body(), document: f.doc })
    );
}

#[test]
fn target_under_superseded_document_is_unresolved() {
    let mut f = fixture();
    let kind = f.store.intern("superseded-document");
    f.store.replace_node(f.doc, Node::Element(kind));
    let block = f.body_blocks()[0];
    let text = f.text_of(block);
    assert_eq!(f.resolve(&[text], TargetKind::Text), Err(AddressError::Unresolved));
    assert_eq!(f.resolve(&[block], TargetKind::Block), Err(AddressError::Unresolved));
    assert_eq!(f.resolve(&[f.body()], TargetKind::Container), Err(AddressError::Unresolved));
}

#[test]
fn block_in_multi_valued_slot_with_collection_parent_still_resolves() {
    // Semantic content links multi-valued slot blocks into a Collection with
    // Child edges; that must not count as a second parent.
    let mut f = fixture();
    let summary = f.slot("summary");
    let first = f.store.children(summary)[0];
    let coll = f.store.add_node(Node::Collection);
    f.store.add_edge(coll, Edge::Child(first));
    assert_eq!(
        f.resolve(&[first], TargetKind::Block),
        Ok(Target::Block { block: first, container: summary, document: f.doc })
    );
}

#[test]
fn block_with_second_element_parent_is_wrong_kind() {
    // Any non-Collection parent counts, so a block shared by two elements is
    // not addressable.
    let mut f = fixture();
    let block = f.body_blocks()[0];
    let kind = f.store.intern("blockquote");
    let other = f.store.add_node(Node::Element(kind));
    f.store.add_edge(other, Edge::Child(block));
    assert!(matches!(
        f.resolve(&[block], TargetKind::Block),
        Err(AddressError::WrongKind { expected: TargetKind::Block, .. })
    ));
}

#[test]
fn set_text_on_html_block_text_is_rejected_and_store_unchanged() {
    // HTML blocks cannot be text-edited (ADR-062 D4).
    let mut f = fixture();
    let body = f.body();
    let html = add_block(
        &mut f.store,
        body,
        &ContentElement::HtmlBlock { source: "<div>x</div>".to_string() },
    );
    let text = f.text_of(html);
    let nodes_before = f.store.node_count();

    let mutation = NedMutation::SetText("<b>y</b>".to_string());
    assert!(matches!(
        f.resolve(&[text], mutation.target_kind()),
        Err(AddressError::WrongKind { expected: TargetKind::Text, .. })
    ));
    assert_eq!(f.store.node_count(), nodes_before);
    assert!(matches!(f.store.get(text), Some(Node::Text(s)) if s == "<div>x</div>"));
}

/// Guard: a text node is not a container.
#[test]
fn insert_child_into_text_node_is_refused() {
    let f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    assert!(matches!(
        f.resolve(&[text], TargetKind::Container),
        Err(AddressError::WrongKind { expected: TargetKind::Container, .. })
    ));
}

/// Guard: frame nodes (body, slot) are not blocks, so they cannot be deleted.
#[test]
fn delete_refuses_frame_node() {
    let f = fixture();
    assert!(matches!(
        f.resolve(&[f.body()], TargetKind::Block),
        Err(AddressError::WrongKind { expected: TargetKind::Block, .. })
    ));
    assert!(matches!(
        f.resolve(&[f.slot("summary")], TargetKind::Block),
        Err(AddressError::WrongKind { expected: TargetKind::Block, .. })
    ));
}

// ── prepare ─────────────────────────────────────────────────────────────────

#[test]
fn set_text_with_same_value_is_no_change() {
    let f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    assert_eq!(f.prepare(text, &NedMutation::SetText("a".to_string())), Err(EditError::NoChange));
}

/// ADR-059 N5: a line break in a heading is a space in its nodes, so a SetText
/// with a newline is accepted and the heading holds one line.
#[test]
fn newline_in_heading_set_text_becomes_a_space() {
    let mut f = fixture();
    let block = f.store.children(f.slot("title"))[0];
    let text = f.text_of(block);
    f.run(text, &NedMutation::SetText("Two\nlines".to_string()));
    assert_eq!(block_from_node(&f.store, block), Ok(heading(1, "Two lines")));
}

/// A fixture whose title slot holds the heading `md`.
fn fixture_with_title(md: &str) -> Fixture {
    let mut store = NodeStore::new();
    let doc = make_element(&mut store, "document");
    let preamble = add_child_element(&mut store, doc, "preamble");
    add_slot(&mut store, preamble, "title", &[heading(1, md)]);
    add_child_element(&mut store, doc, "body");
    Fixture { store, doc }
}

impl Fixture {
    fn title_block(&self) -> NodeId {
        self.store.children(self.slot("title"))[0]
    }
    fn title(&self) -> ContentElement {
        block_from_node(&self.store, self.title_block()).expect("the title decodes")
    }
    fn prepare_scope(&self, m: &NedMutation) -> Result<PreparedEdit, EditError> {
        let target = self.resolve(&[self.title_block()], m.target_kind()).expect("resolves");
        prepare(&self.store, &target, m)
    }
}

/// A Text node inside the emphasis of `Hello *World*`: depth 2.
#[test]
fn set_text_at_depth_in_a_heading() {
    let mut f = fixture_with_title("Hello *World*");
    let heading_block = f.title_block();
    let emph = f.store.children(heading_block)[1];
    let inner = f.store.children(emph)[0];
    assert_eq!(
        f.resolve(&[inner], TargetKind::Text),
        Ok(Target::Text { text: inner, block: heading_block, document: f.doc })
    );
    f.run(inner, &NedMutation::SetText("Presemble".to_string()));
    assert_eq!(f.title(), heading(1, "Hello *Presemble*"));
}

/// A fixture whose body holds the single paragraph `md`.
fn fixture_with_paragraph(md: &str) -> Fixture {
    let mut store = NodeStore::new();
    let doc = make_element(&mut store, "document");
    add_child_element(&mut store, doc, "preamble");
    let body = add_child_element(&mut store, doc, "body");
    add_block(&mut store, body, &para(md));
    Fixture { store, doc }
}

impl Fixture {
    fn paragraph_block(&self) -> NodeId {
        self.store.children(self.body())[0]
    }
    fn paragraph(&self) -> ContentElement {
        block_from_node(&self.store, self.paragraph_block()).expect("the paragraph decodes")
    }
    fn prepare_paragraph_scope(&self, m: &NedMutation) -> Result<PreparedEdit, EditError> {
        let target = self.resolve(&[self.paragraph_block()], m.target_kind()).expect("resolves");
        prepare(&self.store, &target, m)
    }
}

/// A Text node inside the emphasis of `Hello *World*`: depth 2.
#[test]
fn set_text_at_depth_in_a_paragraph() {
    let mut f = fixture_with_paragraph("Hello *World*");
    let block = f.paragraph_block();
    let emph = f.store.children(block)[1];
    let inner = f.store.children(emph)[0];
    assert_eq!(
        f.resolve(&[inner], TargetKind::Text),
        Ok(Target::Text { text: inner, block, document: f.doc })
    );
    f.run(inner, &NedMutation::SetText("Presemble".to_string()));
    assert_eq!(f.paragraph(), para("Hello *Presemble*"));
}

/// The scope of a search in a paragraph is its whole text, across formatting.
#[test]
fn search_replace_on_paragraph_spans_formatting() {
    let mut f = fixture_with_paragraph("a *bc* d");
    let m = NedMutation::SearchReplace { search: "c d".to_string(), replace: "X".to_string() };
    let edit = f.prepare_paragraph_scope(&m).expect("valid");
    commit(&mut f.store, vec![edit]);
    assert_eq!(f.paragraph(), para("a *bX*"));
}

/// L22 (ADR-059 §9): emptying a paragraph is refused; delete it instead.
#[test]
fn emptying_a_paragraph_is_refused() {
    let f = fixture_with_paragraph("Hello");
    let text = f.store.children(f.paragraph_block())[0];
    for m in [
        NedMutation::SetText(String::new()),
        NedMutation::SetText("  \n ".to_string()),
        sr("Hello", ""),
    ] {
        let err = f.prepare(text, &m).expect_err(&format!("{m:?} must be refused"));
        assert!(err.to_string().contains("delete it instead"), "{m:?}: {err}");
    }
    let scope = NedMutation::SearchReplace { search: "Hello".to_string(), replace: String::new() };
    let err = f.prepare_paragraph_scope(&scope).expect_err("refused");
    assert!(err.to_string().contains("delete it instead"), "{err}");
    let err = f.prepare_paragraph_scope(&NedMutation::SetInlines(vec![])).expect_err("refused");
    assert!(err.to_string().contains("delete it instead"), "{err}");
    // A refused edit surfaces as an invalid value, not as a stale selection.
    assert!(matches!(StaleCause::from(err), StaleCause::InvalidValue { .. }));
}

/// A blank line in a paragraph's text would be two paragraphs: refused for
/// every way of writing it, not joined.
#[test]
fn a_blank_line_in_a_paragraph_edit_is_refused() {
    let f = fixture_with_paragraph("Hello *there*");
    let block = f.paragraph_block();
    let text = f.store.children(block)[0];
    assert!(f.prepare(text, &NedMutation::SetText("a\n\nb".to_string())).is_err());
    assert!(f.prepare(text, &sr("Hello", "a\n\nb")).is_err());
    assert!(f.prepare_paragraph_scope(&sr("there", "a\n\nb")).is_err());
    let blank = NodeTree::element("paragraph").with_child(NodeTree::text("a\n\nb"));
    assert!(f.prepare(block, &NedMutation::Replace(vec![blank])).is_err());
}

/// The scope of a search is the heading's whole text, across its formatting.
#[test]
fn search_replace_on_heading_spans_formatting() {
    let mut f = fixture_with_title("a *bc* d");
    let m = NedMutation::SearchReplace { search: "c d".to_string(), replace: "X".to_string() };
    let edit = f.prepare_scope(&m).expect("valid");
    commit(&mut f.store, vec![edit]);
    assert_eq!(f.title(), heading(1, "a *bX*"));
    // Within one Text node it works as before.
    let text = f.store.children(f.title_block())[0];
    let m = NedMutation::SearchReplace { search: "a".to_string(), replace: "Z".to_string() };
    f.run(text, &m);
    assert_eq!(f.title(), heading(1, "Z *bX*"));
}

#[test]
fn search_replace_refuses_match_spanning_atom() {
    let f = fixture_with_title("a ![i](p.png) b");
    let m = NedMutation::SearchReplace {
        search: "a \u{fffc}".to_string(),
        replace: "X".to_string(),
    };
    assert_eq!(f.prepare_scope(&m), Err(EditError::SpansAtom));
    // A match next to the atom is fine.
    let m = NedMutation::SearchReplace { search: "b".to_string(), replace: "X".to_string() };
    assert!(f.prepare_scope(&m).is_ok());
}

#[test]
fn set_inlines_replaces_heading_content() {
    let mut f = fixture_with_title("Old");
    let inlines = content::inline::parse_inlines("New *x*", content::inline::InlineContext::Heading)
        .unwrap();
    let m = NedMutation::SetInlines(crate::inlines_to_tree(&inlines));
    let edit = f.prepare_scope(&m).expect("valid");
    commit(&mut f.store, vec![edit]);
    assert_eq!(f.title(), heading(1, "New *x*"));
}

/// Only an inline container is a text scope: a paragraph block is one, a
/// code block is not.
#[test]
fn a_paragraph_block_is_a_text_scope_and_a_code_block_is_not() {
    let f = fixture();
    let block = f.body_blocks()[0];
    assert!(matches!(
        f.resolve(&[block], TargetKind::TextScope),
        Ok(Target::TextScope { block: b, .. }) if b == block
    ));

    let mut f = fixture();
    let body = f.body();
    let code = add_block(
        &mut f.store,
        body,
        &ContentElement::CodeBlock { language: None, code: "x".to_string() },
    );
    assert!(matches!(
        f.resolve(&[code], TargetKind::TextScope),
        Err(AddressError::WrongKind { expected: TargetKind::TextScope, .. })
    ));
}

/// Guard: a blank line would split a paragraph in two.
#[test]
fn set_text_with_blank_line_in_paragraph_is_refused() {
    let f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    assert!(matches!(
        f.prepare(text, &NedMutation::SetText("a\n\nb".to_string())),
        Err(EditError::Postcondition(RoundTripError::BlockCount(2)))
    ));
}

#[test]
fn set_text_valid_value_is_prepared_on_the_owning_block() {
    let f = fixture();
    let heading = f.store.children(f.slot("title"))[0];
    let text = f.text_of(heading);
    let edit = f.prepare(text, &NedMutation::SetText("New".to_string())).unwrap();
    assert_eq!(edit.footprint(), heading);
    assert_eq!(edit.document(), f.doc);
}

fn sr(search: &str, replace: &str) -> NedMutation {
    NedMutation::SearchReplace { search: search.to_string(), replace: replace.to_string() }
}

#[test]
fn search_replace_zero_occurrences_is_not_found() {
    let f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    assert_eq!(f.prepare(text, &sr("zzz", "y")), Err(EditError::SearchNotFound));
}

#[test]
fn search_replace_two_occurrences_is_not_unique() {
    let mut f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    f.store.replace_node(text, Node::Text("cat and cat".to_string()));
    assert_eq!(
        f.prepare(text, &sr("cat", "dog")),
        Err(EditError::SearchNotUnique { occurrences: 2 })
    );
}

#[test]
fn search_replace_empty_search_is_refused() {
    let f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    assert_eq!(f.prepare(text, &sr("", "y")), Err(EditError::EmptySearch));
}

#[test]
fn search_replace_search_equal_replace_is_no_change() {
    let f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    assert_eq!(f.prepare(text, &sr("a", "a")), Err(EditError::NoChange));
}

#[test]
fn search_replace_unique_writes_the_replaced_text() {
    let mut f = fixture();
    let text = f.text_of(f.body_blocks()[0]);
    f.store.replace_node(text, Node::Text("the cat sat".to_string()));
    f.run(text, &sr("cat", "dog"));
    assert_eq!(f.body_texts(), vec!["the dog sat", "b"]);
}

#[test]
fn replace_with_empty_payload_is_refused() {
    let f = fixture();
    let block = f.body_blocks()[0];
    assert_eq!(f.prepare(block, &NedMutation::Replace(vec![])), Err(EditError::EmptyPayload));
}

#[test]
fn prepare_refuses_empty_payload() {
    let f = fixture();
    let block = f.body_blocks()[0];
    for m in [
        NedMutation::Replace(vec![]),
        NedMutation::InsertBefore(vec![]),
        NedMutation::InsertAfter(vec![]),
    ] {
        assert_eq!(f.prepare(block, &m), Err(EditError::EmptyPayload), "{m:?}");
    }
    assert_eq!(
        f.prepare(f.body(), &NedMutation::InsertChild(vec![])),
        Err(EditError::EmptyPayload)
    );
}

#[test]
fn prepare_refuses_existing_node_payload() {
    let f = fixture();
    let block = f.body_blocks()[0];
    let bare = NodeTree::Existing(f.text_of(block));
    let nested = NodeTree::element("paragraph").with_child(NodeTree::Existing(f.text_of(block)));
    for tree in [bare, nested] {
        assert_eq!(
            f.prepare(block, &NedMutation::InsertAfter(vec![tree])),
            Err(EditError::ExistingNode)
        );
    }
}

#[test]
fn insert_after_html_block_payload_is_refused() {
    // An HTML block can never be inserted through a payload (ADR-062 D4).
    let f = fixture();
    let block = f.body_blocks()[0];
    let tree = NodeTree::element("html-block").with_child(NodeTree::text("<div>x</div>"));
    assert_eq!(
        f.prepare(block, &NedMutation::InsertAfter(vec![tree])),
        Err(EditError::Payload(BlockDecodeError::NotPayloadKind("html-block".to_string())))
    );
}

#[test]
fn image_payload_root_is_refused() {
    let f = fixture();
    let block = f.body_blocks()[0];
    let tree = NodeTree::element("image").with_attr("path", "x.png");
    assert_eq!(
        f.prepare(block, &NedMutation::InsertAfter(vec![tree])),
        Err(EditError::Payload(BlockDecodeError::NotPayloadKind("image".to_string())))
    );
}

#[test]
fn payload_that_does_not_round_trip_is_a_postcondition_failure() {
    let f = fixture();
    let block = f.body_blocks()[0];
    let tree = para_tree("a\n\nb");
    assert!(matches!(
        f.prepare(block, &NedMutation::InsertAfter(vec![tree])),
        Err(EditError::Postcondition(_))
    ));
}

#[test]
fn mutation_kind_mismatch_is_refused() {
    let f = fixture();
    let block = f.body_blocks()[0];
    let target = f.resolve(&[block], TargetKind::Block).unwrap();
    assert_eq!(
        prepare(&f.store, &target, &NedMutation::SetText("x".to_string())),
        Err(EditError::TargetMismatch { expected: TargetKind::Text, found: TargetKind::Block })
    );
}

#[test]
fn prepare_does_not_touch_the_store() {
    let f = fixture();
    let a = f.body_blocks()[0];
    let nodes = f.store.node_count();
    f.prepare(a, &NedMutation::Replace(vec![para_tree("X")])).unwrap();
    f.prepare(a, &NedMutation::Delete).unwrap();
    assert_eq!(f.store.node_count(), nodes);
    assert_eq!(f.body_texts(), vec!["a", "b"]);
}

#[test]
fn prepare_reports_footprint_and_document() {
    let f = fixture();
    let (a, b) = (f.body_blocks()[0], f.body_blocks()[1]);
    let a_text = f.text_of(a);
    let body = f.body();
    let tree = || vec![para_tree("X")];
    let cases: Vec<(NodeId, NedMutation, NodeId)> = vec![
        (a_text, NedMutation::SetText("changed".to_string()), a),
        (a_text, sr("a", "z"), a),
        (b, NedMutation::Replace(tree()), b),
        (b, NedMutation::Delete, b),
        (a, NedMutation::InsertBefore(tree()), a),
        (a, NedMutation::InsertAfter(tree()), a),
        (body, NedMutation::InsertChild(tree()), body),
    ];
    for (target, mutation, footprint) in cases {
        let edit = f.prepare(target, &mutation).unwrap_or_else(|e| panic!("{mutation:?}: {e}"));
        assert_eq!(edit.footprint(), footprint, "{mutation:?}");
        assert_eq!(edit.document(), f.doc, "{mutation:?}");
    }
}

// ── commit ──────────────────────────────────────────────────────────────────

/// Guard: the canonical encoding is written, not the payload's raw attributes.
#[test]
fn insert_stores_canonical_encoding_not_raw_attrs() {
    let mut f = fixture();
    let block = f.body_blocks()[0];
    let tree = NodeTree::element("heading")
        .with_attr("level", "2")
        .with_child(NodeTree::text("Section"));
    f.run(block, &NedMutation::InsertAfter(vec![tree]));
    let new_heading = f.body_blocks()[1];
    let level = f
        .store
        .attributes(new_heading)
        .into_iter()
        .find(|(n, _)| f.store.resolve_name(*n) == "level")
        .and_then(|(_, v)| f.store.get(v).cloned());
    assert!(matches!(level, Some(Node::Integer(2))), "level: {level:?}");
    assert_eq!(block_from_node(&f.store, new_heading), Ok(heading(2, "Section")));
}

#[test]
fn insert_after_two_blocks_keeps_order() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    f.run(a, &NedMutation::InsertAfter(vec![para_tree("X"), para_tree("Y")]));
    assert_eq!(f.body_texts(), vec!["a", "X", "Y", "b"]);
}

#[test]
fn insert_before_keeps_order() {
    let mut f = fixture();
    let b = f.body_blocks()[1];
    f.run(b, &NedMutation::InsertBefore(vec![para_tree("X"), para_tree("Y")]));
    assert_eq!(f.body_texts(), vec!["a", "X", "Y", "b"]);
}

#[test]
fn insert_child_appends_in_order() {
    let mut f = fixture();
    let body = f.body();
    f.run(body, &NedMutation::InsertChild(vec![para_tree("X"), para_tree("Y")]));
    assert_eq!(f.body_texts(), vec!["a", "b", "X", "Y"]);
}

#[test]
fn replace_one_block_with_two() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let a_text = f.text_of(a);
    f.run(a, &NedMutation::Replace(vec![para_tree("X"), para_tree("Y")]));
    assert_eq!(f.body_texts(), vec!["X", "Y", "b"]);
    assert!(f.store.get(a).is_none(), "replaced block removed");
    assert!(f.store.get(a_text).is_none(), "replaced block's text removed");
}

#[test]
fn delete_removes_subtree() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let a_text = f.text_of(a);
    f.run(a, &NedMutation::Delete);
    assert_eq!(f.body_texts(), vec!["b"]);
    assert!(f.store.get(a).is_none());
    assert!(f.store.get(a_text).is_none());
}

fn leading_comments(store: &NodeStore, block: NodeId) -> Vec<String> {
    ned::trivia::trivia_texts(store, block, ned::trivia::ATTR_TRIVIA_BEFORE)
        .into_iter()
        .map(|t| match store.get(t) {
            Some(Node::Text(s)) => s.clone(),
            other => panic!("expected a comment text, got {other:?}"),
        })
        .collect()
}

#[test]
fn replace_keeps_leading_comments_on_the_first_new_root() {
    // ADR-062 D5: the comments of a replaced block move to the first new block.
    let mut f = fixture();
    let a = f.body_blocks()[0];
    add_trivia(&mut f.store, a, ned::trivia::ATTR_TRIVIA_BEFORE, &["<!-- note -->"]);
    f.run(a, &NedMutation::Replace(vec![para_tree("X"), para_tree("Y")]));
    let blocks = f.body_blocks();
    assert_eq!(f.body_texts(), vec!["X", "Y", "b"]);
    assert_eq!(leading_comments(&f.store, blocks[0]), vec!["<!-- note -->"]);
    assert!(leading_comments(&f.store, blocks[1]).is_empty());
    assert!(leading_comments(&f.store, blocks[2]).is_empty());
}

/// Guard: delete keeps the edge while it relocates, so the comments survive.
#[test]
fn delete_moves_leading_comments_to_the_next_sibling() {
    let mut f = fixture();
    let (a, b) = (f.body_blocks()[0], f.body_blocks()[1]);
    add_trivia(&mut f.store, a, ned::trivia::ATTR_TRIVIA_BEFORE, &["<!-- note -->"]);
    f.run(a, &NedMutation::Delete);
    assert_eq!(f.body_blocks(), vec![b]);
    assert_eq!(leading_comments(&f.store, b), vec!["<!-- note -->"]);
}

// Comment trivia on delete (ADR-062 D5), moved from `ned::mutation`: these run
// through `commit`.

const BEFORE: &str = ned::trivia::ATTR_TRIVIA_BEFORE;
const END: &str = ned::trivia::ATTR_TRIVIA_END;

#[test]
fn deleted_blocks_comments_go_before_the_next_siblings_own() {
    let mut f = fixture();
    let (a, b) = (f.body_blocks()[0], f.body_blocks()[1]);
    add_trivia(&mut f.store, a, BEFORE, &["a1"]);
    add_trivia(&mut f.store, b, BEFORE, &["b1"]);
    f.run(a, &NedMutation::Delete);
    assert_eq!(f.body_blocks(), vec![b]);
    assert_eq!(leading_comments(&f.store, b), vec!["a1", "b1"]);
}

#[test]
fn delete_of_the_last_block_moves_comments_to_the_container_end() {
    let mut f = fixture();
    let (a, b) = (f.body_blocks()[0], f.body_blocks()[1]);
    let body = f.body();
    add_trivia(&mut f.store, body, END, &["e0"]);
    add_trivia(&mut f.store, b, BEFORE, &["b1"]);
    f.run(b, &NedMutation::Delete);
    assert_eq!(f.body_blocks(), vec![a]);
    let end: Vec<_> = ned::trivia::trivia_texts(&f.store, body, END)
        .into_iter()
        .map(|t| match f.store.get(t) {
            Some(Node::Text(s)) => s.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(end, vec!["e0", "b1"]);
}

#[test]
fn deleting_adjacent_blocks_in_one_plan_keeps_comment_order() {
    let mut f = fixture();
    let body = f.body();
    let c = add_block(&mut f.store, body, &para("c"));
    let (a, b) = (f.body_blocks()[0], f.body_blocks()[1]);
    add_trivia(&mut f.store, a, BEFORE, &["a1"]);
    add_trivia(&mut f.store, b, BEFORE, &["b1"]);
    add_trivia(&mut f.store, c, BEFORE, &["c1"]);
    let plan: Plan = [
        step(&[a], NedMutation::Delete, "ned/delete"),
        step(&[b], NedMutation::Delete, "ned/delete"),
    ]
    .into_iter()
    .collect();
    execute(&mut f.store, plan).unwrap();
    assert_eq!(f.body_blocks(), vec![c]);
    assert_eq!(leading_comments(&f.store, c), vec!["a1", "b1", "c1"]);
}

#[test]
fn set_text_changes_only_the_target() {
    let mut f = fixture();
    let blocks = f.body_blocks();
    let (a, b) = (blocks[0], blocks[1]);
    let b_text = f.text_of(b);
    let text = f.text_of(a);
    f.run(text, &NedMutation::SetText("changed".to_string()));
    assert_eq!(f.body_texts(), vec!["changed", "b"]);
    assert_eq!(f.text_of(b), b_text, "sibling keeps its text node");
    assert!(matches!(f.store.get(b_text), Some(Node::Text(s)) if s == "b"));
}

#[test]
fn commit_applies_disjoint_edits_in_order() {
    let mut f = fixture();
    let title = f.store.children(f.slot("title"))[0];
    let title_text = f.text_of(title);
    let a = f.body_blocks()[0];
    // Both are prepared against the same, unedited store.
    let set = f.prepare(title_text, &NedMutation::SetText("New title".to_string())).unwrap();
    let insert = f.prepare(a, &NedMutation::InsertAfter(vec![para_tree("X")])).unwrap();
    commit(&mut f.store, vec![set, insert]);
    assert_eq!(f.body_texts(), vec!["a", "X", "b"]);
    assert_eq!(
        block_from_node(&f.store, title),
        Ok(heading(1, "New title"))
    );
}

#[test]
fn commit_reports_documents() {
    let mut f = fixture();
    let other = build_document(&mut f.store);
    let first = f.prepare(f.body_blocks()[0], &NedMutation::Delete).unwrap();
    let body_of_other = find_child_by_name(&f.store, other, "body").unwrap();
    let second = {
        let target = f.resolve(&[body_of_other], TargetKind::Container).unwrap();
        prepare(&f.store, &target, &NedMutation::InsertChild(vec![para_tree("X")])).unwrap()
    };
    let third = f.prepare(f.body_blocks()[1], &NedMutation::InsertAfter(vec![para_tree("Y")])).unwrap();
    let committed = commit(&mut f.store, vec![second, first, third]);
    assert_eq!(committed, Committed { documents: vec![f.doc, other] });
    assert_eq!(commit(&mut f.store, vec![]), Committed { documents: vec![] });
}

#[test]
#[should_panic(expected = "anchor is a child of its container")]
fn commit_with_vanished_anchor_is_a_broken_invariant() {
    // A prepared edit committed to a store that changed under it is a bug, not
    // input: it panics rather than skipping silently.
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let edit = f.prepare(a, &NedMutation::InsertAfter(vec![para_tree("X")])).unwrap();
    let body = f.body();
    f.store.remove_child_edge(body, a);
    commit(&mut f.store, vec![edit]);
}

// ── plans ───────────────────────────────────────────────────────────────────

fn step(ids: &[NodeId], mutation: NedMutation, origin: &'static str) -> PlannedMutation {
    PlannedMutation {
        targets: Selection::from_ids(ids.iter().copied()),
        mutation,
        origin,
    }
}

fn set_text(id: NodeId, value: &str) -> PlannedMutation {
    step(&[id], NedMutation::SetText(value.to_string()), "ned/set-text")
}

fn overlap(entry: usize, origin: &'static str, with: usize, with_origin: &'static str) -> PlanError {
    PlanError { entry, origin, cause: PlanCause::Overlap { with, with_origin } }
}

#[test]
fn plan_with_unmatched_selection_applies_nothing() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let plan: Plan = [
        set_text(f.text_of(a), "changed"),
        step(&[], NedMutation::InsertAfter(vec![para_tree("X")]), "ned/insert-after"),
    ]
    .into_iter()
    .collect();
    let nodes = f.store.node_count();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(PlanError {
            entry: 1,
            origin: "ned/insert-after",
            cause: PlanCause::Address(AddressError::Unresolved)
        })
    );
    assert_eq!(f.body_texts(), vec!["a", "b"]);
    assert_eq!(f.store.node_count(), nodes);
}

#[test]
fn plan_with_ambiguous_selection_applies_nothing() {
    let mut f = fixture();
    let texts: Vec<NodeId> =
        f.store.children(f.slot("summary")).into_iter().map(|b| f.text_of(b)).collect();
    let plan: Plan =
        [step(&texts, NedMutation::SetText("x".to_string()), "ned/set-text")].into_iter().collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(PlanError {
            entry: 0,
            origin: "ned/set-text",
            cause: PlanCause::Address(AddressError::Ambiguous { matches: 2 })
        })
    );
    let summary: Vec<_> = f.store.children(f.slot("summary"));
    assert_eq!(summary.len(), 2);
    assert!(matches!(f.store.get(f.text_of(summary[0])), Some(Node::Text(s)) if s == "One"));
}

#[test]
fn two_edits_in_one_block_are_rejected() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let plan: Plan = [set_text(f.text_of(a), "x"), set_text(f.text_of(a), "y")].into_iter().collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(overlap(1, "ned/set-text", 0, "ned/set-text"))
    );
    assert_eq!(f.body_texts(), vec!["a", "b"]);
}

#[test]
fn insert_after_a_deleted_block_is_rejected() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let plan: Plan = [
        step(&[a], NedMutation::Delete, "ned/delete"),
        step(&[a], NedMutation::InsertAfter(vec![para_tree("X")]), "ned/insert-after"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(overlap(1, "ned/insert-after", 0, "ned/delete"))
    );
    assert_eq!(f.body_texts(), vec!["a", "b"]);
}

#[test]
fn edit_inside_a_replaced_block_is_rejected() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let plan: Plan = [
        step(&[a], NedMutation::Replace(vec![para_tree("X")]), "ned/replace"),
        set_text(f.text_of(a), "y"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(overlap(1, "ned/set-text", 0, "ned/replace"))
    );
    assert_eq!(f.body_texts(), vec!["a", "b"]);
}

#[test]
fn insert_child_and_edit_in_the_same_container_overlap() {
    let mut f = fixture();
    let body = f.body();
    let a_text = f.text_of(f.body_blocks()[0]);
    let append = || {
        step(&[body], NedMutation::InsertChild(vec![para_tree("X")]), "ned/insert-child")
    };
    // The container is an ancestor of the block, whichever comes first.
    let plan: Plan = [append(), set_text(a_text, "y")].into_iter().collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(overlap(1, "ned/set-text", 0, "ned/insert-child"))
    );
    let plan: Plan = [set_text(a_text, "y"), append()].into_iter().collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(overlap(1, "ned/insert-child", 0, "ned/set-text"))
    );
    assert_eq!(f.body_texts(), vec!["a", "b"]);
}

#[test]
fn independent_edits_apply_together() {
    let mut f = fixture();
    let title = f.store.children(f.slot("title"))[0];
    let a = f.body_blocks()[0];
    let plan: Plan = [
        set_text(f.text_of(title), "New title"),
        step(&[a], NedMutation::InsertAfter(vec![para_tree("X")]), "ned/insert-after"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Ok(Executed { documents: vec![f.doc], skipped: vec![] })
    );
    assert_eq!(f.body_texts(), vec!["a", "X", "b"]);
    assert_eq!(block_from_node(&f.store, title), Ok(heading(1, "New title")));
}

#[test]
fn invalid_second_entry_applies_nothing() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let body = f.body();
    let wibble = NodeTree::element("wibble");
    let plan: Plan = [
        set_text(f.text_of(a), "changed"),
        step(&[body], NedMutation::InsertChild(vec![wibble]), "ned/insert-child"),
    ]
    .into_iter()
    .collect();
    let nodes = f.store.node_count();
    let blocks_before: Vec<_> =
        f.body_blocks().into_iter().map(|b| block_from_node(&f.store, b)).collect();
    let err = execute(&mut f.store, plan).unwrap_err();
    assert_eq!((err.entry, err.origin), (1, "ned/insert-child"));
    assert!(matches!(err.cause, PlanCause::Edit(EditError::Payload(_))), "{err:?}");
    assert_eq!(f.store.node_count(), nodes);
    let blocks_after: Vec<_> =
        f.body_blocks().into_iter().map(|b| block_from_node(&f.store, b)).collect();
    assert_eq!(blocks_after, blocks_before);
    assert_eq!(f.body_texts(), vec!["a", "b"]);
}

fn all_texts(f: &Fixture) -> Vec<(NodeId, String)> {
    (0..f.store.node_count() as u64 + 64)
        .map(NodeId)
        .filter_map(|id| match f.store.get(id) {
            Some(Node::Text(s)) => Some((id, s.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn no_change_entry_is_skipped() {
    let mut f = fixture();
    let title = f.store.children(f.slot("title"))[0];
    let a = f.body_blocks()[0];
    // Step 1 sets the title to what it already is; step 2 changes "a".
    let plan: Plan = [set_text(f.text_of(title), "Title"), set_text(f.text_of(a), "changed")]
        .into_iter()
        .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Ok(Executed { documents: vec![f.doc], skipped: vec![0] })
    );
    assert_eq!(f.body_texts(), vec!["changed", "b"]);
}

#[test]
fn all_no_op_plan_commits_nothing() {
    let mut f = fixture();
    let title = f.store.children(f.slot("title"))[0];
    let a = f.body_blocks()[0];
    let plan: Plan = [set_text(f.text_of(title), "Title"), set_text(f.text_of(a), "a")]
        .into_iter()
        .collect();
    let (nodes, texts) = (f.store.node_count(), all_texts(&f));
    assert_eq!(
        execute(&mut f.store, plan),
        Ok(Executed { documents: vec![], skipped: vec![0, 1] })
    );
    assert_eq!(f.store.node_count(), nodes);
    assert_eq!(all_texts(&f), texts);
}

#[test]
fn skipped_entry_takes_no_part_in_overlap() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let plan: Plan = [
        set_text(f.text_of(a), "a"),
        step(&[a], NedMutation::Replace(vec![para_tree("X")]), "ned/replace"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Ok(Executed { documents: vec![f.doc], skipped: vec![0] })
    );
    assert_eq!(f.body_texts(), vec!["X", "b"]);
}

#[test]
fn invalid_entry_after_a_no_op_still_refuses() {
    let mut f = fixture();
    let a = f.body_blocks()[0];
    let plan: Plan = [
        set_text(f.text_of(a), "a"),
        step(&[], NedMutation::InsertAfter(vec![para_tree("X")]), "ned/insert-after"),
    ]
    .into_iter()
    .collect();
    let (nodes, texts) = (f.store.node_count(), all_texts(&f));
    assert_eq!(
        execute(&mut f.store, plan),
        Err(PlanError {
            entry: 1,
            origin: "ned/insert-after",
            cause: PlanCause::Address(AddressError::Unresolved)
        })
    );
    assert_eq!(f.store.node_count(), nodes);
    assert_eq!(all_texts(&f), texts);
}

#[test]
fn overlap_names_the_plan_entry_after_a_skipped_step() {
    let mut f = fixture();
    let title = f.store.children(f.slot("title"))[0];
    let a = f.body_blocks()[0];
    // Step 1 is a no-op and skipped, so "ned/set-text" on "a" is the only
    // prepared edit, yet it is step 2 of the plan (index 1).
    let plan: Plan = [
        set_text(f.text_of(title), "Title"),
        set_text(f.text_of(a), "changed"),
        step(&[a], NedMutation::Replace(vec![para_tree("X")]), "ned/replace"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Err(overlap(2, "ned/replace", 1, "ned/set-text"))
    );
}

#[test]
fn empty_plan_commits_nothing() {
    let mut f = fixture();
    let nodes = f.store.node_count();
    assert!(Plan::default().is_empty());
    assert_eq!(
        execute(&mut f.store, Plan::default()),
        Ok(Executed { documents: vec![], skipped: vec![] })
    );
    assert_eq!(f.store.node_count(), nodes);
}

#[test]
fn plan_error_names_entry_and_origin() {
    let mut f = fixture();
    let texts: Vec<NodeId> =
        f.store.children(f.slot("summary")).into_iter().map(|b| f.text_of(b)).collect();
    let a = f.body_blocks()[0];
    let plan: Plan = [
        set_text(f.text_of(a), "x"),
        step(&texts, NedMutation::SetText("y".to_string()), "ned/set-text"),
    ]
    .into_iter()
    .collect();
    let err = execute(&mut f.store, plan).unwrap_err();
    assert_eq!(
        err.to_string(),
        "step 2 (ned/set-text): the address matches 2 nodes; it must match exactly one"
    );

    let plan: Plan = [
        step(&[a], NedMutation::Replace(vec![para_tree("X")]), "ned/replace"),
        set_text(f.text_of(a), "y"),
    ]
    .into_iter()
    .collect();
    let err = execute(&mut f.store, plan).unwrap_err();
    assert_eq!(
        err.to_string(),
        "step 2 (ned/set-text) changes what step 1 (ned/replace) changes"
    );
    // Both map onto the suggestion surface.
    assert_eq!(StaleCause::from(err.clone()), StaleCause::Overlap { with_step: 1 });
    // And onto the structured refusal: 1-based step, origin, cause, message.
    let failure = editorial_types::PlanFailure::from(err);
    assert_eq!(failure.step, 2);
    assert_eq!(failure.origin, "ned/set-text");
    assert_eq!(failure.cause, StaleCause::Overlap { with_step: 1 });
    assert_eq!(
        failure.message,
        "step 2 (ned/set-text) changes what step 1 (ned/replace) changes"
    );
    let ambiguous = PlanError {
        entry: 0,
        origin: "ned/set-text",
        cause: PlanCause::Address(AddressError::Ambiguous { matches: 2 }),
    };
    assert_eq!(StaleCause::from(ambiguous), StaleCause::Ambiguous { matches: 2 });
    let no_change = PlanError {
        entry: 0,
        origin: "ned/set-text",
        cause: PlanCause::Edit(EditError::NoChange),
    };
    assert_eq!(StaleCause::from(no_change), StaleCause::NoChange);
}

#[test]
fn commit_order_is_plan_order() {
    // Delete A, then InsertBefore B (A's next sibling): A's leading comment
    // moves to B first, and the insert lands before B.
    let mut f = fixture();
    let (a, b) = (f.body_blocks()[0], f.body_blocks()[1]);
    add_trivia(&mut f.store, a, ned::trivia::ATTR_TRIVIA_BEFORE, &["<!-- note -->"]);
    let plan: Plan = [
        step(&[a], NedMutation::Delete, "ned/delete"),
        step(&[b], NedMutation::InsertBefore(vec![para_tree("X")]), "ned/insert-before"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        execute(&mut f.store, plan),
        Ok(Executed { documents: vec![f.doc], skipped: vec![] })
    );
    assert_eq!(f.body_texts(), vec!["X", "b"]);
    let blocks = f.body_blocks();
    assert!(leading_comments(&f.store, blocks[0]).is_empty());
    assert_eq!(blocks[1], b);
    assert_eq!(leading_comments(&f.store, b), vec!["<!-- note -->"]);
}

// ── errors ──────────────────────────────────────────────────────────────────

#[test]
fn edit_error_maps_to_stale_cause() {
    let invalid = |e: EditError| StaleCause::InvalidValue { detail: e.to_string() };
    let rows: Vec<(EditError, StaleCause)> = vec![
        (
            EditError::TargetMismatch { expected: TargetKind::Text, found: TargetKind::Block },
            StaleCause::InvalidValue {
                detail: "the target kind does not match the mutation".to_string(),
            },
        ),
        (EditError::ExistingNode, invalid(EditError::ExistingNode)),
        (
            EditError::EmptyPayload,
            StaleCause::InvalidValue { detail: "empty payload".to_string() },
        ),
        (
            EditError::Payload(BlockDecodeError::NotAnElement),
            invalid(EditError::Payload(BlockDecodeError::NotAnElement)),
        ),
        (
            EditError::OwningBlock(BlockDecodeError::NotAnElement),
            invalid(EditError::OwningBlock(BlockDecodeError::NotAnElement)),
        ),
        (
            EditError::Postcondition(RoundTripError::BlockCount(2)),
            invalid(EditError::Postcondition(RoundTripError::BlockCount(2))),
        ),
        (
            EditError::EmptySearch,
            StaleCause::InvalidValue { detail: "empty search text".to_string() },
        ),
        (EditError::SearchNotFound, StaleCause::SearchNotFound),
        (
            EditError::SearchNotUnique { occurrences: 3 },
            StaleCause::SearchNotUnique { occurrences: 3 },
        ),
        (EditError::NoChange, StaleCause::NoChange),
    ];
    for (error, cause) in rows {
        assert_eq!(StaleCause::from(error.clone()), cause, "{error:?}");
    }
}

#[test]
fn address_errors_convert_to_stale_causes() {
    assert_eq!(
        StaleCause::from(AddressError::Ambiguous { matches: 3 }),
        StaleCause::Ambiguous { matches: 3 }
    );
    assert_eq!(
        AddressError::Ambiguous { matches: 3 }.to_string(),
        StaleCause::Ambiguous { matches: 3 }.to_string()
    );
}
