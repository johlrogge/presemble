//! Auto-save (ADR-070): which dirty documents to save on a tick, as a pure
//! policy over what the conductor knows about each of them. The conductor
//! gathers the facts, asks [`due`], and carries the decisions out through its
//! ordinary save path.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where the conductor reads the time from: the system clock, or a clock a
/// test moves by hand (so tests never sleep).
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The system's monotonic clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Whether ticks save, and after how long without a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoSaveConfig {
    /// Off: ticks save nothing (an explicit save still does).
    pub enabled: bool,
    /// How long a document must go unchanged before a tick saves it.
    pub idle: Duration,
}

impl Default for AutoSaveConfig {
    fn default() -> Self {
        AutoSaveConfig {
            enabled: true,
            idle: Duration::from_millis(2000),
        }
    }
}

impl AutoSaveConfig {
    /// How long a document that waits for its editor's canonical text waits
    /// before the editor is asked again: five idle periods.
    pub fn ask_again_after(&self) -> Duration {
        self.idle * 5
    }
}

/// What an open editor of a document means for saving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorView {
    /// No editor has the document open.
    Closed,
    /// The editor has a region N does not hold yet (held or drifted): the
    /// document is not auto-saved until it is settled.
    Unsettled,
    /// The editor's text is N's canonical text: saving writes what it shows.
    Canonical,
    /// The editor's text is not N's canonical text yet; `asked` is when it was
    /// last asked for it (`None`: not yet).
    Waiting { asked: Option<Instant> },
}

/// What the conductor knows about one dirty document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocView {
    /// Site-relative path of the document.
    pub path: PathBuf,
    /// When it last changed, from any channel (`None`: not known, so it is
    /// not waiting for anything).
    pub last_change: Option<Instant>,
    /// In conflict with its file on disk (ADR-065): never auto-saved.
    pub conflicted: bool,
    pub editor: EditorView,
}

/// What a tick does for one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Save it through the ordinary save path.
    Save(PathBuf),
    /// Ask its open editor for N's canonical text
    /// (`ConductorEvent::EditorCanonicalWanted`).
    AskForCanonical(PathBuf),
}

/// The decisions of a tick at `now`.
///
/// Nothing when auto-save is off. Otherwise each document that has been
/// unchanged for the idle time, and is not in conflict with its file, is
/// saved, unless an editor has it open:
/// - a held or drifted region in the editor: it waits until that is settled;
/// - the editor shows N's canonical text: it is saved;
/// - not yet: the editor is asked for it, and asked again every
///   [`AutoSaveConfig::ask_again_after`] while the document waits. It is
///   never saved on a timer of its own.
pub fn due(now: Instant, docs: &[DocView], config: &AutoSaveConfig) -> Vec<Decision> {
    if !config.enabled {
        return Vec::new();
    }
    let elapsed = |since: Instant| now.saturating_duration_since(since);
    docs.iter()
        .filter(|doc| !doc.conflicted)
        .filter(|doc| doc.last_change.is_none_or(|at| elapsed(at) >= config.idle))
        .filter_map(|doc| match doc.editor {
            EditorView::Closed | EditorView::Canonical => Some(Decision::Save(doc.path.clone())),
            EditorView::Unsettled => None,
            EditorView::Waiting { asked } => asked
                .is_none_or(|at| elapsed(at) >= config.ask_again_after())
                .then(|| Decision::AskForCanonical(doc.path.clone())),
        })
        .collect()
}

/// The documents a flush saves (`Command::FlushDirty`, and shutdown): every
/// one that can be saved now, without waiting for the idle time or for an
/// open editor's canonical text (it is saved as N has it). A document in
/// conflict with its file, or whose editor has a region N does not hold yet,
/// is still not saved.
pub fn flushable(docs: &[DocView]) -> Vec<PathBuf> {
    docs.iter()
        .filter(|doc| !doc.conflicted && doc.editor != EditorView::Unsettled)
        .map(|doc| doc.path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_millis(2000);

    fn doc(last_change: Instant, editor: EditorView) -> DocView {
        DocView {
            path: PathBuf::from("content/post/a.md"),
            last_change: Some(last_change),
            conflicted: false,
            editor,
        }
    }

    fn decide(now: Instant, doc: DocView) -> Vec<Decision> {
        due(now, &[doc], &AutoSaveConfig::default())
    }

    fn save() -> Vec<Decision> {
        vec![Decision::Save(PathBuf::from("content/post/a.md"))]
    }

    fn ask() -> Vec<Decision> {
        vec![Decision::AskForCanonical(PathBuf::from(
            "content/post/a.md",
        ))]
    }

    #[test]
    fn a_document_idle_for_exactly_the_idle_time_is_saved() {
        let t = Instant::now();
        assert_eq!(decide(t + IDLE, doc(t, EditorView::Closed)), save());
        assert_eq!(
            decide(
                t + IDLE - Duration::from_millis(1),
                doc(t, EditorView::Closed)
            ),
            vec![]
        );
    }

    #[test]
    fn a_conflicted_document_is_never_decided_on() {
        let t = Instant::now();
        for editor in [
            EditorView::Closed,
            EditorView::Canonical,
            EditorView::Waiting { asked: None },
        ] {
            let conflicted = DocView {
                conflicted: true,
                ..doc(t, editor)
            };
            assert_eq!(decide(t + IDLE * 100, conflicted), vec![], "{editor:?}");
        }
    }

    #[test]
    fn an_editor_decides_between_save_wait_and_ask() {
        let t = Instant::now();
        let now = t + IDLE;
        assert_eq!(decide(now, doc(t, EditorView::Canonical)), save());
        assert_eq!(decide(now, doc(t, EditorView::Unsettled)), vec![]);
        assert_eq!(
            decide(now, doc(t, EditorView::Waiting { asked: None })),
            ask()
        );
    }

    #[test]
    fn a_waiting_editor_is_asked_again_after_five_idle_times() {
        let t = Instant::now();
        let asked = t + IDLE;
        let waiting = doc(t, EditorView::Waiting { asked: Some(asked) });
        let again = AutoSaveConfig::default().ask_again_after();
        assert_eq!(again, IDLE * 5);
        assert_eq!(
            decide(asked + again - Duration::from_millis(1), waiting.clone()),
            vec![]
        );
        assert_eq!(decide(asked + again, waiting), ask());
    }

    #[test]
    fn auto_save_off_decides_nothing() {
        let t = Instant::now();
        let off = AutoSaveConfig {
            enabled: false,
            ..AutoSaveConfig::default()
        };
        assert_eq!(
            due(t + IDLE * 100, &[doc(t, EditorView::Closed)], &off),
            vec![]
        );
    }

    #[test]
    fn a_flush_skips_only_conflicted_and_unsettled_documents() {
        let t = Instant::now();
        let at = |name: &str, conflicted: bool, editor: EditorView| DocView {
            path: PathBuf::from(name),
            last_change: Some(t),
            conflicted,
            editor,
        };
        let docs = [
            at("closed", false, EditorView::Closed),
            at("canonical", false, EditorView::Canonical),
            at("waiting", false, EditorView::Waiting { asked: None }),
            at("unsettled", false, EditorView::Unsettled),
            at("conflicted", true, EditorView::Closed),
        ];
        assert_eq!(
            flushable(&docs),
            ["closed", "canonical", "waiting"].map(PathBuf::from)
        );
    }

    #[test]
    fn a_document_without_a_known_change_time_is_not_held_back() {
        let t = Instant::now();
        let unknown = DocView {
            last_change: None,
            ..doc(t, EditorView::Closed)
        };
        assert_eq!(decide(t, unknown), save());
    }
}
