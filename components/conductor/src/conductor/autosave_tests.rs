//! Auto-save on ticks (ADR-070, A1): the conductor side, with a clock the
//! tests move by hand.

use super::*;

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::autosave::{AutoSaveConfig, Clock};
use editorial_types::{BlockEditIntent, BlockText, ContentPath};

const FILE: &str = "content/post/first.md";
const SCHEMA: &str =
    "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";
const TEMPLATE: &str = r#"<html><body><presemble:insert data="(ref input.title)" as="h1"></presemble:insert><div><presemble:insert data="(ref input.body)"></presemble:insert></div></body></html>"#;
const CONTENT: &str =
    "# First Post\n\n----\n\nAlpha para.\n\nBeta para.\n\n- one\n- two\n- three\n";
const IDLE: Duration = Duration::from_millis(2000);
const OTHER: &str = "content/post/second.md";
const OTHER_CONTENT: &str = "# Second Post\n\n----\n\nGamma para.\n";

/// A clock that only moves when the test says so.
struct TestClock(Mutex<Instant>);

impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

struct World {
    tmp: tempfile::TempDir,
    clock: Arc<TestClock>,
    conductor: Conductor,
}

impl World {
    fn new() -> World {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        for dir in ["schemas/post", "templates/post", "content/post"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("schemas/post/item.md"), SCHEMA).unwrap();
        std::fs::write(root.join("templates/post/item.html"), TEMPLATE).unwrap();
        std::fs::write(root.join(FILE), CONTENT).unwrap();
        std::fs::write(root.join(OTHER), OTHER_CONTENT).unwrap();
        let repo = site_repository::SiteRepository::builder()
            .from_dir(root)
            .build();
        let clock = Arc::new(TestClock(Mutex::new(Instant::now())));
        let conductor = Conductor::with_repo(root.to_path_buf(), repo)
            .expect("conductor")
            .with_clock(clock.clone());
        World {
            tmp,
            clock,
            conductor,
        }
    }

    fn advance(&self, by: Duration) {
        *self.clock.0.lock().unwrap() += by;
    }

    fn tick(&self) -> CommandResult {
        self.conductor.handle_command(Command::AutoSaveTick)
    }

    /// Change the title in N, as the browser or MCP would.
    fn set_title(&self, text: &str) {
        self.set_title_of(FILE, text);
    }

    /// Change the title of the document at `file` in N.
    fn set_title_of(&self, file: &str, text: &str) {
        let program = format!(
            r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{file}") "title"))) "{text}")"#
        );
        assert!(
            matches!(
                self.conductor.apply_ned_program(&program).response,
                Response::Applied { dirty_paths: 1, .. }
            ),
            "the edit applies"
        );
    }

    fn on_disk(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join(FILE)).unwrap()
    }

    fn dirty(&self) -> bool {
        self.conductor
            .dirty_docs
            .read()
            .unwrap()
            .contains(Path::new(FILE))
    }

    fn conflicted(&self) -> bool {
        self.conductor
            .dirty_docs
            .read()
            .unwrap()
            .is_conflicted(Path::new(FILE))
    }

    /// N's canonical text of the document.
    fn canonical(&self) -> String {
        let root = self
            .conductor
            .doc_root_for_path(Path::new(FILE))
            .expect("in N");
        let store = self.conductor.node_store.read().unwrap();
        node_store_bridge::serialize_from_store(&store, root).expect("serializes")
    }

    fn open_editor(&self, text: &str) {
        self.conductor.handle_command(Command::EditorOpened {
            path: path(),
            text: text.to_string(),
        });
    }

    fn editor_types(&self, text: &str) -> CommandResult {
        self.conductor.handle_command(Command::EditorChanged {
            path: path(),
            text: text.to_string(),
        })
    }
}

fn path() -> ContentPath {
    ContentPath::parse(FILE).expect("valid")
}

fn saved(result: &CommandResult) -> bool {
    result.events.iter().any(
        |e| matches!(e, ConductorEvent::DocumentsSaved { files } if files.iter().any(|f| f == FILE)),
    )
}

fn asks_for_canonical(result: &CommandResult) -> usize {
    result
        .events
        .iter()
        .filter(|e| matches!(e, ConductorEvent::EditorCanonicalWanted { file } if file == FILE))
        .count()
}

/// An editor whose text reads as N but is not its canonical text: it changed
/// "Alpha para." (which reaches N) and writes the list with `*`.
fn waiting_editor(w: &World) -> String {
    w.open_editor(CONTENT);
    let text = CONTENT
        .replace("Alpha para.", "Alpha changed.")
        .replace("- ", "* ");
    w.editor_types(&text);
    assert!(w.dirty(), "the editor's change reached N");
    assert_ne!(text, w.canonical(), "the editor's text is not canonical");
    text
}

#[test]
fn a_dirty_document_is_saved_after_it_has_been_idle() {
    let w = World::new();
    w.set_title("Saved Title");
    w.advance(IDLE);

    let result = w.tick();

    assert!(saved(&result), "{:?}", result.events);
    assert!(w.on_disk().contains("# Saved Title"), "{}", w.on_disk());
    assert!(!w.dirty());
}

#[test]
fn a_document_still_being_edited_is_not_saved_yet() {
    let w = World::new();
    w.set_title("Saved Title");
    w.advance(IDLE - Duration::from_millis(1));

    let result = w.tick();

    assert!(!saved(&result), "{:?}", result.events);
    assert_eq!(w.on_disk(), CONTENT);
    assert!(w.dirty());
}

#[test]
fn every_new_change_restarts_the_idle_time() {
    let w = World::new();
    w.set_title("First Edit");
    w.advance(IDLE * 3 / 4);
    w.set_title("Second Edit");
    w.advance(IDLE * 3 / 4);

    let early = w.tick();
    assert!(!saved(&early), "idle since the first change only");
    assert_eq!(w.on_disk(), CONTENT);

    w.advance(IDLE / 4);
    let result = w.tick();
    assert!(saved(&result), "{:?}", result.events);
    assert!(w.on_disk().contains("# Second Edit"));
}

/// Change the title in N and on disk, differently: the document conflicts
/// with its file. Returns the file's text and what the watcher's report did.
fn conflict(w: &World) -> (String, CommandResult) {
    w.set_title("Edited Here");
    let outside = CONTENT.replace("# First Post", "# Edited Outside");
    std::fs::write(w.tmp.path().join(FILE), &outside).unwrap();
    let reported = w.conductor.handle_command(Command::FileChanged {
        paths: vec![editorial_types::SitePath::parse(FILE).expect("valid")],
    });
    assert!(w.conflicted(), "the title changed on both sides");
    (outside, reported)
}

/// An editor that changed "Alpha para." (in N now) and then opened a fence
/// it has not closed: the blocks after it are held back.
fn held_editor(w: &World) {
    w.open_editor(CONTENT);
    let changed = CONTENT.replace("Alpha para.", "Alpha changed.");
    w.editor_types(&changed);
    let held = changed.replace("Beta para.", "```rust\nlet x = 1;\n\nBeta para.");
    let result = w.editor_types(&held);
    assert!(
        matches!(&result.response, Response::EditorSynced { held, .. } if !held.is_empty()),
        "{:?}",
        result.response
    );
    assert!(w.dirty());
}

/// The browser changes "Alpha para." in N, and the editor, which has not seen
/// that, changes the same block: its version drifts.
fn drifted_editor(w: &World) {
    w.open_editor(CONTENT);
    let browser = w
        .conductor
        .handle_command(Command::BlockEdit(editorial_types::BlockEditRequest {
            file: path(),
            selection: format!(
                r#"(ned/nth-of-kind (ned/children (ned/body (ned/doc-by-path "{FILE}"))) "paragraph" 0)"#
            ),
            base: BlockText::Markdown("Alpha para.".to_string()),
            proposed: BlockText::Markdown("Alpha browser.".to_string()),
            intent: BlockEditIntent::Apply,
            reason: None,
        }));
    assert!(matches!(browser.response, Response::BlockEdited(_)));
    let result = w.editor_types(&CONTENT.replace("Alpha para.", "Alpha mine."));
    assert!(
        matches!(&result.response, Response::EditorSynced { drifts, .. } if !drifts.is_empty()),
        "{:?}",
        result.response
    );
    assert!(w.dirty());
}

#[test]
fn a_document_in_a_disk_conflict_is_never_auto_saved() {
    let w = World::new();
    let (outside, _) = conflict(&w);

    for _ in 0..10 {
        w.advance(IDLE * 10);
        let result = w.tick();
        assert!(!saved(&result), "{:?}", result.events);
    }
    assert_eq!(w.on_disk(), outside, "the file is left as it is");
    assert!(w.conflicted());
}

#[test]
fn a_document_with_a_held_region_waits() {
    let w = World::new();
    held_editor(&w);

    w.advance(IDLE * 100);
    let result = w.tick();

    assert!(!saved(&result), "{:?}", result.events);
    assert_eq!(asks_for_canonical(&result), 0, "nothing to ask for yet");
    assert_eq!(w.on_disk(), CONTENT);
    assert!(w.dirty());
}

#[test]
fn a_document_with_a_drifted_region_waits() {
    let w = World::new();
    drifted_editor(&w);

    w.advance(IDLE * 100);
    let result = w.tick();

    assert!(!saved(&result), "{:?}", result.events);
    assert_eq!(asks_for_canonical(&result), 0);
    assert_eq!(w.on_disk(), CONTENT);
    assert!(w.dirty());
}

#[test]
fn an_open_editors_document_waits_for_canonical_text_then_saves() {
    let w = World::new();
    waiting_editor(&w);
    w.advance(IDLE);

    let first = w.tick();
    assert!(!saved(&first), "{:?}", first.events);
    assert_eq!(asks_for_canonical(&first), 1, "{:?}", first.events);
    let again = w.tick();
    assert_eq!(
        asks_for_canonical(&again),
        0,
        "asked once, not on every tick"
    );
    assert!(!saved(&again));

    // The language server brings the buffer to the canonical text.
    let canonical = w.canonical();
    w.editor_types(&canonical);
    let early = w.tick();
    assert!(!saved(&early), "the buffer just changed");

    w.advance(IDLE);
    let result = w.tick();
    assert!(saved(&result), "{:?}", result.events);
    assert_eq!(w.on_disk(), canonical, "the file is what the editor shows");
    assert!(!w.dirty());
}

#[test]
fn an_open_editors_document_is_never_auto_saved_before_its_text_is_canonical() {
    let w = World::new();
    waiting_editor(&w);

    for _ in 0..100 {
        w.advance(IDLE);
        let result = w.tick();
        assert!(!saved(&result), "{:?}", result.events);
    }
    assert_eq!(w.on_disk(), CONTENT);
    assert!(w.dirty(), "still unsaved");
}

#[test]
fn the_editor_is_asked_again_while_the_document_waits() {
    let w = World::new();
    waiting_editor(&w);
    w.advance(IDLE);
    assert_eq!(asks_for_canonical(&w.tick()), 1, "asked once it is idle");

    let config = AutoSaveConfig::default();
    w.advance(config.ask_again_after() - Duration::from_millis(1));
    assert_eq!(asks_for_canonical(&w.tick()), 0, "not yet");

    w.advance(Duration::from_millis(1));
    let result = w.tick();
    assert_eq!(asks_for_canonical(&result), 1, "asked again");
    assert!(!saved(&result));

    w.advance(config.ask_again_after());
    assert_eq!(asks_for_canonical(&w.tick()), 1, "and again");
    assert_eq!(w.on_disk(), CONTENT, "never saved");
}

/// The editor typed after it was asked (the canonical edits were dropped, as
/// they were computed for a text it has left): its next pause asks again at
/// once, not 5×idle after the last ask.
#[test]
fn a_new_pause_after_a_dropped_canonical_edit_asks_again_at_once() {
    let w = World::new();
    let typed = waiting_editor(&w);
    w.advance(IDLE);
    assert_eq!(asks_for_canonical(&w.tick()), 1, "asked once it is idle");

    // The user types on; the canonical edits are dropped.
    w.editor_types(&typed.replace("Alpha changed.", "Alpha changed again."));
    w.advance(IDLE - Duration::from_millis(1));
    assert_eq!(asks_for_canonical(&w.tick()), 0, "still typing");

    w.advance(Duration::from_millis(1));
    let result = w.tick();
    assert_eq!(asks_for_canonical(&result), 1, "the new pause asks at once");
    assert!(!saved(&result));
}

#[test]
fn closing_the_editor_lets_the_document_auto_save() {
    let w = World::new();
    waiting_editor(&w);
    w.advance(IDLE);
    assert!(!saved(&w.tick()));

    w.conductor
        .handle_command(Command::EditorClosed { path: path() });
    let result = w.tick();

    assert!(saved(&result), "{:?}", result.events);
    assert!(w.on_disk().contains("Alpha changed."), "{}", w.on_disk());
    assert!(!w.dirty());
}

#[test]
fn invalid_content_is_auto_saved() {
    let w = World::new();
    // The schema wants the title capitalized.
    w.set_title("lower case title");
    w.advance(IDLE);

    let result = w.tick();

    assert!(saved(&result), "{:?}", result.events);
    assert!(
        w.on_disk().contains("# lower case title"),
        "{}",
        w.on_disk()
    );
}

#[test]
fn with_auto_save_off_nothing_is_saved_by_ticks() {
    let w = World::new();
    w.conductor.set_autosave(AutoSaveConfig {
        enabled: false,
        ..AutoSaveConfig::default()
    });
    w.set_title("Saved Title");

    for _ in 0..10 {
        w.advance(IDLE * 10);
        let result = w.tick();
        assert!(!saved(&result), "{:?}", result.events);
    }
    assert_eq!(w.on_disk(), CONTENT);

    // An explicit save still saves.
    let result = w.conductor.handle_command(Command::SaveBuffer {
        path: editorial_types::SitePath::parse(FILE).expect("valid"),
        consent: Default::default(),
    });
    assert!(saved(&result), "{:?}", result.response);
    assert!(w.on_disk().contains("# Saved Title"));
}

/// Make the document's file read-only (`true`) or writable again (`false`).
/// Whether a write to it now fails: as root a read-only file is still
/// writable, and the caller skips.
fn read_only(w: &World, read_only: bool) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let file = w.tmp.path().join(FILE);
    let mode = if read_only { 0o444 } else { 0o644 };
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    let fails = std::fs::OpenOptions::new().write(true).open(&file).is_err();
    if read_only && !fails {
        eprintln!("skipped: the read-only file is writable (root)");
    }
    fails
}

#[test]
fn a_failed_save_stays_dirty_and_is_retried() {
    let w = World::new();
    w.set_title("Saved Title");
    if !read_only(&w, true) {
        return;
    }
    w.advance(IDLE);

    let failed = w.tick();
    assert!(
        matches!(&failed.response, Response::Error(e) if e.contains(FILE)),
        "{:?}",
        failed.response
    );
    assert!(!saved(&failed));
    assert!(w.dirty(), "the document stays unsaved");
    assert!(!w.conflicted(), "a failed write is no conflict");
    assert_eq!(w.on_disk(), CONTENT);

    read_only(&w, false);
    let retried = w.tick();
    assert!(saved(&retried), "{:?}", retried.response);
    assert!(w.on_disk().contains("# Saved Title"));
    assert!(!w.dirty());
}

#[test]
fn an_auto_save_merges_an_outside_change_first() {
    let w = World::new();
    w.set_title("Edited Here");
    // Another program changes another block; the watcher has not said so.
    let outside = CONTENT.replace("Beta para.", "Beta outside.");
    std::fs::write(w.tmp.path().join(FILE), &outside).unwrap();
    w.advance(IDLE);

    let result = w.tick();

    assert!(saved(&result), "{:?}", result.events);
    let disk = w.on_disk();
    assert!(disk.contains("# Edited Here"), "our edit: {disk}");
    assert!(disk.contains("Beta outside."), "the outside change: {disk}");
    assert!(!w.dirty());
}

// ── A4: bringing the editor to the canonical text ──────────────────────────

impl World {
    fn editor_answer(&self, cmd: Command) -> (Option<crate::protocol::TextHash>, String) {
        match self.conductor.handle_command(cmd).response {
            Response::EditorUpdate { last_hash, edits } => {
                (last_hash, apply_wire_edits(&self.editor_last(), &edits))
            }
            other => panic!("expected an editor update, got {other:?}"),
        }
    }

    fn editor_last(&self) -> String {
        self.conductor
            .editors
            .read()
            .unwrap()
            .get(&path())
            .map(|state| state.last.clone())
            .expect("an open editor")
    }
}

fn apply_wire_edits(text: &str, edits: &[crate::protocol::BufferEdit]) -> String {
    let edits: Vec<content::sync::BufferEdit> = edits
        .iter()
        .map(|e| content::sync::BufferEdit {
            range: e.range.clone(),
            text: e.text.clone(),
        })
        .collect();
    content::sync::apply_edits(text, &edits)
}

/// The editor update of LSP-1 leaves text that reads as N alone, so it does
/// not bring an editor to the canonical text: `GetEditorCanonical` does.
#[test]
fn get_editor_canonical_takes_the_editors_text_to_the_canonical_text() {
    let w = World::new();
    let typed = waiting_editor(&w);

    let (update_hash, updated) = w.editor_answer(Command::GetEditorUpdate { path: path() });
    assert_eq!(update_hash, Some(crate::protocol::TextHash::of(&typed)));
    assert_eq!(updated, typed, "the update leaves the `*` bullets alone");

    let (hash, canonical) = w.editor_answer(Command::GetEditorCanonical { path: path() });
    assert_eq!(hash, Some(crate::protocol::TextHash::of(&typed)));
    assert_eq!(canonical, w.canonical());

    // The editor takes it; the next tick after the idle time saves.
    w.editor_types(&canonical);
    w.advance(IDLE);
    let result = w.tick();
    assert!(saved(&result), "{:?}", result.events);
    assert_eq!(w.on_disk(), canonical);
}

#[test]
fn get_editor_canonical_leaves_an_unsettled_editor_alone() {
    let held = World::new();
    held_editor(&held);
    let drifted = World::new();
    drifted_editor(&drifted);
    for (what, w) in [("held", &held), ("drifted", &drifted)] {
        let (hash, text) = w.editor_answer(Command::GetEditorCanonical { path: path() });
        assert!(hash.is_some(), "{what}");
        assert_eq!(text, w.editor_last(), "{what}: no edits");
    }

    // No open editor: nothing.
    let closed = World::new();
    match closed
        .conductor
        .handle_command(Command::GetEditorCanonical { path: path() })
        .response
    {
        Response::EditorUpdate { last_hash, edits } => {
            assert_eq!(last_hash, None);
            assert!(edits.is_empty());
        }
        other => panic!("{other:?}"),
    }
}

// ── A2: flushing, shutdown, save state ─────────────────────────────────────

impl World {
    fn flush(&self, file: Option<&str>) -> CommandResult {
        self.conductor.handle_command(Command::FlushDirty {
            file: file.map(|f| ContentPath::parse(f).expect("valid")),
        })
    }

    fn on_disk_of(&self, file: &str) -> String {
        std::fs::read_to_string(self.tmp.path().join(file)).unwrap()
    }
}

/// The save states the result announces for `file`, in order.
fn save_states(result: &CommandResult, file: &str) -> Vec<crate::protocol::SaveState> {
    result
        .events
        .iter()
        .filter_map(|e| match e {
            ConductorEvent::SaveState { file: f, state } if f == file => Some(state.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn flush_saves_a_document_at_once() {
    let w = World::new();
    w.set_title("Flushed Title");

    let result = w.flush(Some(FILE));

    assert!(
        matches!(result.response, Response::Ok),
        "{:?}",
        result.response
    );
    assert!(saved(&result), "{:?}", result.events);
    assert!(w.on_disk().contains("# Flushed Title"), "{}", w.on_disk());
    assert!(!w.dirty());
}

#[test]
fn flush_saves_without_waiting_for_the_editors_canonical_text() {
    let w = World::new();
    waiting_editor(&w);

    let result = w.flush(Some(FILE));

    assert!(saved(&result), "{:?}", result.events);
    assert!(w.on_disk().contains("Alpha changed."), "{}", w.on_disk());
    assert!(!w.dirty());
}

#[test]
fn flush_without_a_file_saves_every_dirty_document() {
    let w = World::new();
    w.set_title("First Flushed");
    w.set_title_of(OTHER, "Second Flushed");

    let result = w.flush(None);

    assert!(
        matches!(result.response, Response::Ok),
        "{:?}",
        result.response
    );
    assert!(w.on_disk().contains("# First Flushed"), "{}", w.on_disk());
    assert!(
        w.on_disk_of(OTHER).contains("# Second Flushed"),
        "{}",
        w.on_disk_of(OTHER)
    );
    assert!(!w.dirty());
}

#[test]
fn flush_of_one_file_leaves_the_others_unsaved() {
    let w = World::new();
    w.set_title("First Flushed");
    w.set_title_of(OTHER, "Second Unsaved");

    w.flush(Some(FILE));

    assert!(w.on_disk().contains("# First Flushed"));
    assert_eq!(w.on_disk_of(OTHER), OTHER_CONTENT);
}

#[test]
fn flush_still_skips_conflicted_held_and_drifted_documents() {
    let conflicted = World::new();
    let (outside, _) = conflict(&conflicted);
    let held = World::new();
    held_editor(&held);
    let drifted = World::new();
    drifted_editor(&drifted);

    for (what, w, disk) in [
        ("conflicted", &conflicted, outside.as_str()),
        ("held", &held, CONTENT),
        ("drifted", &drifted, CONTENT),
    ] {
        for file in [Some(FILE), None] {
            let result = w.flush(file);
            assert!(!saved(&result), "{what}: {:?}", result.events);
            assert_eq!(w.on_disk(), disk, "{what}: the file is left as it is");
            assert!(w.dirty(), "{what}: still unsaved");
        }
    }
}

fn auto_save_off(w: &World) {
    w.conductor.set_autosave(AutoSaveConfig {
        enabled: false,
        ..AutoSaveConfig::default()
    });
}

#[test]
fn with_auto_save_off_flush_saves_nothing() {
    let w = World::new();
    auto_save_off(&w);
    w.set_title("Not Flushed");
    w.set_title_of(OTHER, "Not Flushed Either");

    for file in [Some(FILE), None] {
        let result = w.flush(file);
        assert!(
            matches!(result.response, Response::Ok),
            "{:?}",
            result.response
        );
        assert!(!saved(&result), "{:?}", result.events);
    }
    assert_eq!(w.on_disk(), CONTENT);
    assert_eq!(w.on_disk_of(OTHER), OTHER_CONTENT);
    assert!(w.dirty());
}

#[test]
fn with_auto_save_off_shutdown_warns_instead_of_saving() {
    let w = World::new();
    auto_save_off(&w);
    w.set_title("Not Saved At Shutdown");
    w.set_title_of(OTHER, "Not Saved Either");

    let result = w.conductor.handle_command(Command::Shutdown);

    assert!(!saved(&result), "{:?}", result.events);
    assert_eq!(w.on_disk(), CONTENT);
    assert_eq!(w.on_disk_of(OTHER), OTHER_CONTENT);
    let Response::Error(warning) = &result.response else {
        panic!("expected a warning, got {:?}", result.response);
    };
    for file in [FILE, OTHER] {
        assert!(
            warning.contains(&format!("unsaved edits to {file} are lost")),
            "{warning}"
        );
    }
}

#[test]
fn with_auto_save_off_an_explicit_save_still_saves() {
    let w = World::new();
    auto_save_off(&w);

    w.set_title("Saved One");
    let one = w.conductor.handle_command(Command::SaveBuffer {
        path: editorial_types::SitePath::parse(FILE).expect("valid"),
        consent: Default::default(),
    });
    assert!(saved(&one), "SaveBuffer: {:?}", one.response);
    assert!(w.on_disk().contains("# Saved One"));

    w.set_title("Saved All");
    let all = w.conductor.handle_command(Command::SaveAllBuffers {
        consent: Default::default(),
    });
    assert!(saved(&all), "SaveAllBuffers: {:?}", all.response);
    assert!(w.on_disk().contains("# Saved All"));

    // The editor's `:w`: it writes its text, then says so.
    let text = w.on_disk().replace("Alpha para.", "Alpha written.");
    w.open_editor(&w.on_disk());
    w.editor_types(&text);
    assert!(w.dirty());
    std::fs::write(w.tmp.path().join(FILE), &text).unwrap();
    w.conductor.handle_command(Command::DocumentSaved {
        path: editorial_types::SitePath::parse(FILE).expect("valid"),
    });
    assert!(!w.dirty(), ":w saved the document");
    assert_eq!(w.on_disk(), text);
}

#[test]
fn shutdown_flushes_before_stopping() {
    let w = World::new();
    w.set_title("Saved At Shutdown");
    waiting_editor(&w);

    let result = w.conductor.handle_command(Command::Shutdown);

    assert!(
        matches!(result.response, Response::Ok),
        "{:?}",
        result.response
    );
    assert!(saved(&result), "{:?}", result.events);
    let disk = w.on_disk();
    assert!(disk.contains("# Saved At Shutdown"), "{disk}");
    assert!(disk.contains("Alpha changed."), "{disk}");
    assert!(!w.dirty());
}

#[test]
fn shutdown_warns_about_each_conflicted_document() {
    let w = World::new();
    let (outside, _) = conflict(&w);
    w.set_title_of(OTHER, "Saved At Shutdown");

    let result = w.conductor.handle_command(Command::Shutdown);

    // The other document is saved; the conflicted one is not.
    assert!(w.on_disk_of(OTHER).contains("# Saved At Shutdown"));
    assert_eq!(w.on_disk(), outside);
    let Response::Error(warning) = &result.response else {
        panic!("expected a warning, got {:?}", result.response);
    };
    assert!(warning.contains(FILE), "names the file: {warning}");
    assert!(
        warning.contains("unsaved edits will be lost"),
        "says what happens to them: {warning}"
    );
    assert!(!warning.contains(OTHER), "only the lost ones: {warning}");
}

#[test]
fn save_state_goes_unsaved_saved_and_failed() {
    use crate::protocol::SaveState;

    let w = World::new();
    let edited = w.conductor.apply_ned_program(&format!(
        r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{FILE}") "title"))) "Edited Once")"#
    ));
    assert_eq!(save_states(&edited, FILE), [SaveState::Unsaved]);
    let again = w.conductor.apply_ned_program(&format!(
        r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{FILE}") "title"))) "Edited Twice")"#
    ));
    assert_eq!(save_states(&again, FILE), [], "already unsaved");

    let flushed = w.flush(Some(FILE));
    assert!(
        matches!(
            save_states(&flushed, FILE).as_slice(),
            [SaveState::Saved { .. }]
        ),
        "{:?}",
        flushed.events
    );

    w.set_title("Edited Thrice");
    if !read_only(&w, true) {
        return;
    }
    let failed = w.flush(Some(FILE));
    assert!(
        matches!(save_states(&failed, FILE).as_slice(), [SaveState::Failed { error }] if !error.is_empty()),
        "{:?}",
        failed.events
    );
    read_only(&w, false);

    // An auto-save is a save like any other.
    w.advance(IDLE);
    let ticked = w.tick();
    assert!(
        matches!(
            save_states(&ticked, FILE).as_slice(),
            [SaveState::Saved { .. }]
        ),
        "{:?}",
        ticked.events
    );
}

#[test]
fn a_disk_conflict_announces_the_conflict_state() {
    use crate::protocol::SaveState;

    let w = World::new();
    let (_, reported) = conflict(&w);
    assert_eq!(save_states(&reported, FILE), [SaveState::Conflict]);

    // Reloading takes the file: nothing is unsaved any more.
    let reloaded = w
        .conductor
        .handle_command(Command::ReloadDocument { path: path() });
    assert!(
        matches!(
            save_states(&reloaded, FILE).as_slice(),
            [SaveState::Saved { .. }]
        ),
        "{:?}",
        reloaded.events
    );
}
