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
        let program = format!(
            r#"(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "{FILE}") "title"))) "{text}")"#
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

#[test]
fn a_document_in_a_disk_conflict_is_never_auto_saved() {
    let w = World::new();
    w.set_title("Edited Here");
    let outside = CONTENT.replace("# First Post", "# Edited Outside");
    std::fs::write(w.tmp.path().join(FILE), &outside).unwrap();
    w.conductor.handle_command(Command::FileChanged {
        paths: vec![editorial_types::SitePath::parse(FILE).expect("valid")],
    });
    assert!(w.conflicted(), "the title changed on both sides");

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
    w.open_editor(CONTENT);
    let changed = CONTENT.replace("Alpha para.", "Alpha changed.");
    w.editor_types(&changed);
    // An unclosed fence: the blocks after it are held back.
    let held = changed.replace("Beta para.", "```rust\nlet x = 1;\n\nBeta para.");
    let result = w.editor_types(&held);
    assert!(
        matches!(&result.response, Response::EditorSynced { held, .. } if !held.is_empty()),
        "{:?}",
        result.response
    );
    assert!(w.dirty());

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
    // The editor, which has not seen the browser's edit, changes the block.
    let result = w.editor_types(&CONTENT.replace("Alpha para.", "Alpha mine."));
    assert!(
        matches!(&result.response, Response::EditorSynced { drifts, .. } if !drifts.is_empty()),
        "{:?}",
        result.response
    );

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

#[test]
fn a_failed_save_stays_dirty_and_is_retried() {
    use std::os::unix::fs::PermissionsExt;

    let w = World::new();
    w.set_title("Saved Title");
    let file = w.tmp.path().join(FILE);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().write(true).open(&file).is_ok() {
        // Running as root: a read-only file can still be written.
        eprintln!("skipped: the read-only file is writable (root)");
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

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
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
