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
    if !matches!(mutation, NedMutation::Replace(_) | NedMutation::SetText(_)) {
        return Ok(());
    }
    let block = match target {
        Target::Text { block, .. } | Target::Block { block, .. } => *block,
        Target::Container { .. } => return Ok(()),
    };
    let base = editorial_types::BlockText::Markdown(preview.base.markdown.clone());
    match node_store_bridge::block_edit::check_base(store, block, &base) {
        Ok(_) => Ok(()),
        Err(e) if e.cause == BlockEditCause::Drifted => Err(StaleCause::Drifted),
        Err(e) => Err(StaleCause::InvalidValue { detail: e.message }),
    }
}

// ── Creation hints ──────────────────────────────────────────────────────────

/// A short, actionable hint for a suggestion rejected at creation time.
pub(crate) fn creation_hint(cause: &StaleCause, kind: TargetKind) -> Option<&'static str> {
    match cause {
        StaleCause::WrongKind { expected: TargetKind::Text, found }
            if BLOCK_KINDS.contains(&found.as_str()) =>
        {
            Some("address the block's text with (ned/text-in <block address>)")
        }
        StaleCause::WrongKind { expected: TargetKind::Text, found }
            if found == SLOT_KIND || found == BODY_KIND || found == UNASSIGNED_KIND =>
        {
            Some(
                "address one block's text with (ned/text-in (ned/children <slot address>)) \
                 or (ned/text-in (ned/nth-of-kind (ned/children <slot>) \"paragraph\" i))",
            )
        }
        StaleCause::Ambiguous { .. } if kind == TargetKind::Text => Some(
            "the address matches several text nodes; narrow it to one block, e.g. (ned/text-in (ned/nth-of-kind (ned/children <slot>) \"paragraph\" i)), or use a longer search string so it occurs in only one block",
        ),
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
