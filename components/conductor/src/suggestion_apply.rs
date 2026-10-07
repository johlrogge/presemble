//! The suggestion-specific parts of applying NED suggestion mutations
//! (ADR-056): drift, and the hints for a suggestion rejected at creation.
//!
//! Resolving a selection, preparing a mutation and committing it are typed
//! editorial operations of `content_store::edit`. The conductor runs all
//! three under a single store write lock at accept time, and resolve and
//! prepare under a read lock at creation time.

use content_store::edit::{BODY_KIND, SLOT_KIND, Target, UNASSIGNED_KIND};
use content_store::BLOCK_KINDS;
use editorial_types::{BlockEditCause, NedMutation, StaleCause, SuggestionPreview, TargetKind};
use node_store::NodeStore;

pub(crate) use content_store::edit::DOCUMENT_KIND;

// ── Drift ───────────────────────────────────────────────────────────────────

/// Whether the block a block-edit suggestion was made against still reads as
/// its `preview.base` (ADR-059 §10).
///
/// Only `Replace` and `SetText` are checked: they overwrite the whole block
/// or its whole text. `SearchReplace` keeps its own local drift detection (the
/// search text must still occur exactly once) and gets no base check, as do
/// the other mutations. The comparison is `block_edit::check_base`, the one
/// definition of drift, shared with the lowering.
pub(crate) fn check_drift(
    store: &NodeStore,
    target: &Target,
    mutation: &NedMutation,
    preview: &SuggestionPreview,
) -> Result<(), StaleCause> {
    if !matches!(
        mutation,
        NedMutation::Replace(_) | NedMutation::SetText(_) | NedMutation::SetInlines(_)
    ) {
        return Ok(());
    }
    let block = match target {
        Target::Text { block, .. }
        | Target::TextScope { block, .. }
        | Target::Block { block, .. } => *block,
        Target::Container { .. } => return Ok(()),
    };
    let base = editorial_types::BlockText::Markdown(preview.base.markdown.clone());
    match node_store_bridge::block_edit::check_base(store, block, &base) {
        Ok(_) => Ok(()),
        Err(e) if e.cause == BlockEditCause::Drifted => Err(StaleCause::Drifted),
        Err(e) => Err(StaleCause::InvalidValue { detail: e.message }),
    }
}

// ── Already in place ────────────────────────────────────────────────────────

/// Whether the block a block-edit suggestion was made against already reads
/// as its `preview.proposed`: the change has landed another way, so there is
/// nothing left to apply.
///
/// Only mutations that rewrite the addressed block (`SetText`,
/// `SearchReplace`, `Replace`) can be in place this way. An insert next to a
/// block that happens to read as the proposal has not been applied, so it is
/// never "in place".
///
/// The comparison is `block_edit::check_base` against the proposal, the same
/// definition of "reads as" that drift uses. It covers single-block proposals
/// only: a proposal of several blocks never reads as one block, so it is not
/// recognised here and falls through to the drift check. A structural
/// suggestion that is already in place therefore goes Stale with `Drifted`.
pub(crate) fn already_in_place(
    store: &NodeStore,
    target: &Target,
    mutation: &NedMutation,
    preview: &SuggestionPreview,
) -> bool {
    if !matches!(
        mutation,
        NedMutation::SetText(_)
            | NedMutation::SearchReplace { .. }
            | NedMutation::SetInlines(_)
            | NedMutation::Replace(_)
    ) {
        return false;
    }
    let block = match target {
        Target::Text { block, .. }
        | Target::TextScope { block, .. }
        | Target::Block { block, .. } => *block,
        Target::Container { .. } => return false,
    };
    // A replacement is compared as the whole block, not as its markdown: the
    // projection of a slot heading has no level, so markdown cannot tell that
    // the level changed.
    if let NedMutation::Replace(trees) = mutation {
        return node_store_bridge::block_edit::block_reads_as_trees(store, block, trees);
    }
    let proposed = editorial_types::BlockText::Markdown(preview.proposed.markdown.clone());
    node_store_bridge::block_edit::check_base(store, block, &proposed).is_ok()
}

// ── Creation hints ──────────────────────────────────────────────────────────

/// A short, actionable hint for a suggestion rejected at creation time.
pub(crate) fn creation_hint(cause: &StaleCause, kind: TargetKind) -> Option<&'static str> {
    match cause {
        StaleCause::WrongKind { expected: TargetKind::Text | TargetKind::TextScope, found }
            if BLOCK_KINDS.contains(&found.as_str()) =>
        {
            Some("address the block's text with (ned/text-in <block address>)")
        }
        StaleCause::WrongKind { expected: TargetKind::Text | TargetKind::TextScope, found }
            if found == SLOT_KIND || found == BODY_KIND || found == UNASSIGNED_KIND =>
        {
            Some(
                "address one block's text with (ned/text-in (ned/children <slot address>)) \
                 or (ned/text-in (ned/nth-of-kind (ned/children <slot>) \"paragraph\" i))",
            )
        }
        StaleCause::Ambiguous { .. }
            if matches!(kind, TargetKind::Text | TargetKind::TextScope) =>
        {
            Some(
                "the address matches several text nodes; narrow it to one block, e.g. (ned/text-in (ned/nth-of-kind (ned/children <slot>) \"paragraph\" i)), or use a longer search string so it occurs in only one block",
            )
        }
        StaleCause::Unresolved => {
            Some("check the file path and slot name; empty slots need InsertChild on the slot")
        }
        StaleCause::SearchNotUnique { .. } => {
            Some("include more surrounding words so the search text occurs once")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_next_to_a_block_that_reads_as_the_proposal_is_not_in_place() {
        use content::{Document, ContentElement};
        use node_store_bridge::content_bridge::{DocumentMeta, document_to_store, find_child_by_name};
        use schema::{Span, Spanned};

        let doc = Document {
            preamble: im::vector![],
            body: im::vector![Spanned {
                node: ContentElement::Paragraph { text: "X".to_string() },
                span: Span { start: 0, end: 0 },
            }],
            has_separator: true,
            separator_span: None,
            unassigned: Default::default(),
            comments: Default::default(),
        };
        let meta = DocumentMeta {
            url: "/post/x".to_string(),
            stem: "post".to_string(),
            file: "content/post/x.md".to_string(),
            page_kind: "item".to_string(),
        };
        let mut store = NodeStore::new();
        let root = document_to_store(&doc, &mut store, Some(&meta));
        let body = find_child_by_name(&store, root, "body").unwrap();
        let block = store.children(body)[0];
        let target = content_store::edit::resolve(
            &store,
            &ned::Selection::single(block),
            TargetKind::Block,
        )
        .unwrap();
        let preview = SuggestionPreview {
            base: editorial_types::PreviewText::new("X"),
            proposed: editorial_types::PreviewText::new("X"),
        };
        let paragraph = ned::NodeTree::element("paragraph").with_child(ned::NodeTree::text("X"));

        // The anchor reads as the proposal, but the inserted block is not there.
        let insert = NedMutation::InsertAfter(vec![paragraph]);
        assert!(!already_in_place(&store, &target, &insert, &preview));
        // The same block under a rewriting mutation is in place.
        let replace = NedMutation::SetText("X".to_string());
        let text_target = content_store::edit::resolve(
            &store,
            &ned::Selection::single(store.children(block)[0]),
            TargetKind::Text,
        )
        .unwrap();
        assert!(already_in_place(&store, &text_target, &replace, &preview));
    }

    #[test]
    fn hints_match_causes() {
        let wrong = StaleCause::WrongKind { expected: TargetKind::Text, found: "heading".into() };
        assert!(creation_hint(&wrong, TargetKind::Text).unwrap().contains("ned/text-in"));
        let slot = StaleCause::WrongKind { expected: TargetKind::Text, found: "slot".into() };
        assert!(creation_hint(&slot, TargetKind::Text).unwrap().contains("ned/text-in"));
        let unassigned =
            StaleCause::WrongKind { expected: TargetKind::Text, found: "unassigned".into() };
        assert!(creation_hint(&unassigned, TargetKind::Text).unwrap().contains("ned/text-in"));
        let amb = StaleCause::Ambiguous { matches: 2 };
        assert!(creation_hint(&amb, TargetKind::Text).unwrap().contains("ned/nth-of-kind"));
        assert!(creation_hint(&amb, TargetKind::Block).is_none());
        assert!(creation_hint(&StaleCause::Unresolved, TargetKind::Block).is_some());
        assert!(
            creation_hint(&StaleCause::SearchNotUnique { occurrences: 2 }, TargetKind::Text)
                .is_some()
        );
        assert!(creation_hint(&StaleCause::NoChange, TargetKind::Text).is_none());
    }
}
