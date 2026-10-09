//! The pure parts of the language server's editor sync (LSP-2, 2.5): what is
//! sent to the conductor and when, and what the conductor's answers become in
//! the editor (diagnostics and code actions). Nothing here is tied to a real
//! conductor or to the client: the line to the conductor is a [`Link`] that
//! the server supplies (and a test replaces with a slow fake).
//!
//! Positions are LSP positions: lines, and UTF-16 code units in a line.

use conductor::{Drift, DriftId, DriftKind, HeldRegion};
use content::sync::BufferEdit;
use serde_json::json;
use tower_lsp::lsp_types::*;

// ── The sender ──────────────────────────────────────────────────────────────

/// The line to the conductor: one blocking request and its answer.
pub trait Link: Send + 'static {
    fn send(&mut self, cmd: &conductor::Command) -> Result<conductor::Response, String>;
}

/// What runs when the conductor has answered (or failed to).
pub type Done = Box<dyn FnOnce(Result<conductor::Response, String>) + Send>;

struct Job {
    /// The file the job is about (coalescing is per file).
    key: String,
    /// Whether a newer text of the same file may replace this job while it waits.
    latest_wins: bool,
    cmd: conductor::Command,
    done: Done,
}

#[derive(Default)]
struct State {
    jobs: std::collections::VecDeque<Job>,
    /// A send is in flight.
    busy: bool,
    stopped: bool,
}

struct Shared {
    state: std::sync::Mutex<State>,
    /// Signalled when a job is queued, a send completes, or the sender stops.
    wake: std::sync::Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The one line to the conductor, off the handlers. Everything for the
/// conductor goes through one queue, so that what the editor did arrives in the
/// order it was done; a handler queues its job and returns at once, and a
/// worker thread does the blocking sends.
///
/// A new text of a file replaces the one before it while that still waits
/// (`push_latest`), unless something else for the same file was queued in
/// between (a save or a close): then both are sent, in order. A text is never
/// replaced once its send has begun.
pub struct Sender {
    shared: std::sync::Arc<Shared>,
}

impl Sender {
    /// Start the worker thread on `link`.
    pub fn start(mut link: impl Link) -> Sender {
        let shared = std::sync::Arc::new(Shared { state: Default::default(), wake: Default::default() });
        let worker = shared.clone();
        std::thread::spawn(move || loop {
            let job = {
                let mut state = worker.lock();
                loop {
                    if let Some(job) = state.jobs.pop_front() {
                        state.busy = true;
                        break job;
                    }
                    if state.stopped {
                        return;
                    }
                    state = worker.wake.wait(state).unwrap_or_else(|e| e.into_inner());
                }
            };
            let reply = link.send(&job.cmd);
            (job.done)(reply);
            worker.lock().busy = false;
            worker.wake.notify_all();
        });
        Sender { shared }
    }

    /// Queue a job behind everything queued so far.
    pub fn push(&self, key: &str, cmd: conductor::Command, done: Done) {
        self.queue(Job { key: key.to_string(), latest_wins: false, cmd, done });
    }

    /// Queue a text of a file: it replaces the newest job of that file if that
    /// is a text that still waits.
    pub fn push_latest(&self, key: &str, cmd: conductor::Command, done: Done) {
        self.queue(Job { key: key.to_string(), latest_wins: true, cmd, done });
    }

    fn queue(&self, job: Job) {
        let mut state = self.shared.lock();
        let newest_of_file = state.jobs.iter_mut().rev().find(|j| j.key == job.key);
        match newest_of_file {
            Some(waiting) if job.latest_wins && waiting.latest_wins => *waiting = job,
            _ => state.jobs.push_back(job),
        }
        drop(state);
        self.shared.wake.notify_all();
    }

    /// Wait until nothing is queued and nothing is in flight.
    #[cfg(test)]
    pub fn wait_idle(&self) {
        let mut state = self.shared.lock();
        while !state.jobs.is_empty() || state.busy {
            state = self.shared.wake.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_all();
    }
}

/// The conflict actions to offer for a code action request: both, wherever the
/// cursor is, while the document is in conflict with the file on disk.
pub fn conflict_offer(
    _request: &Range,
    conflicted: bool,
    uri: &Url,
    rel: &str,
    version: i32,
    buffer: &str,
    file: &str,
) -> Vec<CodeActionOrCommand> {
    if conflicted {
        conflict_actions(uri, rel, version, buffer, file)
    } else {
        Vec::new()
    }
}

// ── What the conductor answered ─────────────────────────────────────────────

/// The conductor's last answer for a buffer, with the text and the version it
/// is about (the byte ranges of the answer are in `text`).
#[derive(Debug, Clone)]
pub struct Synced {
    pub version: i32,
    pub text: String,
    pub drifts: Vec<Drift>,
    pub held: Vec<HeldRegion>,
}

/// The commands the code actions run (`workspace/executeCommand`).
pub const KEEP_MINE: &str = "presemble.keepMine";
pub const RELOAD_DOCUMENT: &str = "presemble.reloadDocument";
pub const OVERWRITE_FILE: &str = "presemble.overwriteFile";

/// The titles of the code actions.
pub const TAKE_CURRENT: &str = "Take the current version";
pub const KEEP_MINE_TITLE: &str = "Keep mine";
pub const TAKE_FILE: &str = "Take the file on disk (discard unsaved edits)";
pub const KEEP_EDITS: &str = "Keep the unsaved edits (overwrite the file)";
pub const MATCH_FILE: &str = "Match the file on disk";

/// The LSP range of the bytes `range` of `text`.
pub fn lsp_range(text: &str, range: &std::ops::Range<usize>) -> Range {
    let (start_line, start_character) = content::byte_to_position(text, range.start);
    let (end_line, end_character) = content::byte_to_position(text, range.end);
    Range {
        start: Position { line: start_line, character: start_character },
        end: Position { line: end_line, character: end_character },
    }
}

/// The diagnostics for what the conductor did not take: a drift is a warning
/// on its range, a held region is information.
pub fn sync_diagnostics(synced: &Synced) -> Vec<Diagnostic> {
    let drifts = synced.drifts.iter().map(|drift| Diagnostic {
        range: lsp_range(&synced.text, &drift.range),
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("presemble".to_string()),
        message: match drift.kind {
            DriftKind::Blocks => {
                "This block was changed elsewhere since you started editing it, so your version was not applied."
            }
            DriftKind::ListSpacing => {
                "This list's spacing changed here while one of its items was changed elsewhere."
            }
        }
        .to_string(),
        ..Default::default()
    });
    let held = synced.held.iter().map(|region| Diagnostic {
        range: lsp_range(&synced.text, &region.range),
        severity: Some(DiagnosticSeverity::INFORMATION),
        source: Some("presemble".to_string()),
        message: held_message(&region.reason),
        ..Default::default()
    });
    drifts.chain(held).collect()
}

/// What a held region says to the author.
pub fn held_message(reason: &str) -> String {
    let what = if reason.contains("fence") { "code fence" } else { "HTML block" };
    format!("This {what} isn't closed yet; the blocks after it are unchanged until you close it.")
}

fn overlaps(a: &Range, b: &Range) -> bool {
    !(a.end.line < b.start.line
        || (a.end.line == b.start.line && a.end.character < b.start.character)
        || b.end.line < a.start.line
        || (b.end.line == a.start.line && b.end.character < a.start.character))
}

/// A workspace edit that applies `edits` to the document at `version`: if the
/// buffer has moved on, the client refuses it.
pub fn versioned_edit(uri: &Url, version: i32, edits: Vec<TextEdit>) -> WorkspaceEdit {
    WorkspaceEdit {
        document_changes: Some(DocumentChanges::Edits(vec![TextDocumentEdit {
            text_document: OptionalVersionedTextDocumentIdentifier { uri: uri.clone(), version: Some(version) },
            edits: edits.into_iter().map(OneOf::Left).collect(),
        }])),
        ..Default::default()
    }
}

fn command(title: &str, command: &str, arguments: Vec<serde_json::Value>) -> Command {
    Command { title: title.to_string(), command: command.to_string(), arguments: Some(arguments) }
}

/// The code actions for the drifts that overlap `request`: take the current
/// version (a versioned edit of just the range), or keep mine (the conductor
/// takes the editor's version).
pub fn sync_actions(uri: &Url, synced: &Synced, request: &Range) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();
    for drift in &synced.drifts {
        let range = lsp_range(&synced.text, &drift.range);
        if !overlaps(&range, request) {
            continue;
        }
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: TAKE_CURRENT.to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            edit: Some(versioned_edit(
                uri,
                synced.version,
                vec![TextEdit { range, new_text: drift.current.clone() }],
            )),
            ..Default::default()
        }));
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: KEEP_MINE_TITLE.to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            command: Some(command(
                KEEP_MINE_TITLE,
                KEEP_MINE,
                vec![json!(uri.to_string()), json!(drift.id.to_string())],
            )),
            ..Default::default()
        }));
    }
    actions
}

/// The drift id spelled in a command argument (`DriftId`'s `Display`).
pub fn parse_drift_id(spelled: &str) -> Option<DriftId> {
    u64::from_str_radix(spelled, 16).ok().map(DriftId)
}

// ── A conflict with the file on disk (ADR-065) ─────────────────────────────

/// The warning on a document whose file changed on disk while it has unsaved
/// edits. It says where things stand, not where the edits came from.
pub fn conflict_diagnostic() -> Diagnostic {
    Diagnostic {
        range: Range::default(),
        severity: Some(DiagnosticSeverity::WARNING),
        source: Some("presemble".to_string()),
        message: "This file changed on disk while the document has unsaved edits. Take the file on disk, or keep the unsaved edits and overwrite the file."
            .to_string(),
        ..Default::default()
    }
}

/// The two ways out of a conflict. Taking the file also replaces the buffer
/// with the file's text, through a versioned edit: without it the next
/// keystroke would bring the discarded edits back. `rel` is the site-relative
/// path of the file.
pub fn conflict_actions(
    uri: &Url,
    rel: &str,
    version: i32,
    buffer: &str,
    file: &str,
) -> Vec<CodeActionOrCommand> {
    let edits = text_edits(buffer, &content::sync::format_edits(buffer, file));
    vec![
        CodeActionOrCommand::CodeAction(CodeAction {
            title: TAKE_FILE.to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            edit: (!edits.is_empty()).then(|| versioned_edit(uri, version, edits)),
            command: Some(command(TAKE_FILE, RELOAD_DOCUMENT, vec![json!(rel)])),
            ..Default::default()
        }),
        CodeActionOrCommand::CodeAction(CodeAction {
            title: KEEP_EDITS.to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            command: Some(command(KEEP_EDITS, OVERWRITE_FILE, vec![json!(rel)])),
            ..Default::default()
        }),
    ]
}

// ── Spelling ────────────────────────────────────────────────────────────────

/// LSP edits for byte edits of `text`.
pub fn text_edits(text: &str, edits: &[BufferEdit]) -> Vec<TextEdit> {
    edits
        .iter()
        .map(|e| TextEdit { range: lsp_range(text, &e.range), new_text: e.text.clone() })
        .collect()
}

/// The edits that make `buffer` the text of `file`, when the two read as the
/// same content but are spelled differently (`* a` and `- a`). `None` when the
/// texts are the same, or when the content differs: then it is not a matter of
/// spelling and there is nothing to match.
pub fn match_file_edits(buffer: &str, file: &str) -> Option<Vec<TextEdit>> {    if buffer == file || !content::sync::changed_block_edits(buffer, file).is_empty() {
        return None;
    }
    Some(text_edits(buffer, &content::sync::format_edits(buffer, file)))
}

/// The edits that make `src` canonical: the text N would write for it. Empty
/// when it is canonical already, or cannot be read.
pub fn formatting_edits(src: &str, grammar: &schema::Grammar) -> Vec<TextEdit> {
    let Ok(doc) = content::parse_and_assign(src, grammar) else { return Vec::new() };
    let canonical = content::serialize_document(&doc);
    text_edits(src, &content::sync::format_edits(src, &canonical))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri() -> Url {
        Url::parse("file:///site/content/post/a.md").expect("a url")
    }

    /// The text with LSP `edits` applied (positions in UTF-16 code units).
    fn apply(text: &str, edits: &[TextEdit]) -> String {
        let offset = |p: Position| -> usize {
            let mut line_start = 0;
            for _ in 0..p.line {
                line_start += text[line_start..].find('\n').map_or(text.len() - line_start, |i| i + 1);
            }
            let mut units = 0;
            for (i, c) in text[line_start..].char_indices() {
                if units >= p.character as usize {
                    return line_start + i;
                }
                units += c.len_utf16();
            }
            text.len()
        };
        let mut ordered: Vec<&TextEdit> = edits.iter().collect();
        ordered.sort_by_key(|e| (e.range.start.line, e.range.start.character));
        let mut out = String::new();
        let mut at = 0;
        for edit in ordered {
            out.push_str(&text[at..offset(edit.range.start)]);
            out.push_str(&edit.new_text);
            at = offset(edit.range.end);
        }
        out.push_str(&text[at..]);
        out
    }

    /// The edits of a code action's workspace edit, with the version they are for.
    fn edits_of(action: &CodeActionOrCommand) -> (Option<i32>, Vec<TextEdit>) {
        let CodeActionOrCommand::CodeAction(action) = action else { panic!("a code action") };
        let edit = action.edit.as_ref().expect("an edit");
        match edit.document_changes.as_ref().expect("document changes") {
            DocumentChanges::Edits(changes) => {
                assert_eq!(changes.len(), 1);
                let edits = changes[0]
                    .edits
                    .iter()
                    .map(|e| match e {
                        OneOf::Left(edit) => edit.clone(),
                        OneOf::Right(annotated) => annotated.text_edit.clone(),
                    })
                    .collect();
                (changes[0].text_document.version, edits)
            }
            other => panic!("expected edits, got {other:?}"),
        }
    }

    fn title(action: &CodeActionOrCommand) -> &str {
        match action {
            CodeActionOrCommand::CodeAction(a) => &a.title,
            CodeActionOrCommand::Command(c) => &c.title,
        }
    }

    fn command_of(action: &CodeActionOrCommand) -> &Command {
        match action {
            CodeActionOrCommand::CodeAction(a) => a.command.as_ref().expect("a command"),
            CodeActionOrCommand::Command(c) => c,
        }
    }

    // ── the sender ──────────────────────────────────────────────────────────

    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn changed(text: &str) -> conductor::Command {
        conductor::Command::EditorChanged {
            path: conductor::editorial_types::ContentPath::parse("content/post/a.md").expect("a content path"),
            text: text.to_string(),
        }
    }

    fn closed() -> conductor::Command {
        conductor::Command::EditorClosed {
            path: conductor::editorial_types::ContentPath::parse("content/post/a.md").expect("a content path"),
        }
    }

    fn saved() -> conductor::Command {
        conductor::Command::DocumentSaved {
            path: conductor::editorial_types::SitePath::parse("content/post/a.md").expect("a site path"),
        }
    }

    /// A conductor whose first answer is slow: it says it has begun, then waits
    /// until it is let go. Everything it is sent is written to the log.
    struct Slow {
        log: Arc<Mutex<Vec<String>>>,
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        first: bool,
    }

    impl Link for Slow {
        fn send(&mut self, cmd: &conductor::Command) -> Result<conductor::Response, String> {
            let line = match cmd {
                conductor::Command::EditorChanged { text, .. } => format!("changed {text}"),
                conductor::Command::EditorClosed { .. } => "closed".to_string(),
                conductor::Command::DocumentSaved { .. } => "saved".to_string(),
                other => format!("{other:?}"),
            };
            self.log.lock().unwrap().push(line);
            if self.first {
                self.first = false;
                self.started.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(conductor::Response::Ok)
        }
    }

    struct Rig {
        sender: Sender,
        log: Arc<Mutex<Vec<String>>>,
        replies: Arc<Mutex<Vec<String>>>,
        started: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
    }

    fn rig() -> Rig {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let sender = Sender::start(Slow { log: log.clone(), started: started_tx, release: release_rx, first: true });
        Rig { sender, log, replies: Arc::new(Mutex::new(Vec::new())), started, release }
    }

    impl Rig {
        fn done(&self, name: &str) -> Done {
            let replies = self.replies.clone();
            let name = name.to_string();
            Box::new(move |_| replies.lock().unwrap().push(name))
        }

        /// Wait until the first send is in flight.
        fn first_send_in_flight(&self) {
            self.started.recv_timeout(Duration::from_secs(2)).expect("the first send began");
        }

        fn finish(&self) -> Vec<String> {
            self.release.send(()).unwrap();
            self.sender.wait_idle();
            self.log.lock().unwrap().clone()
        }
    }

    #[test]
    fn a_burst_during_a_slow_send_sends_only_the_latest_text() {
        let rig = rig();
        rig.sender.push_latest("a", changed("1"), rig.done("1"));
        rig.first_send_in_flight();
        for n in 2..=5 {
            rig.sender.push_latest("a", changed(&n.to_string()), rig.done(&n.to_string()));
        }
        assert_eq!(rig.finish(), ["changed 1", "changed 5"], "exactly two sends, the second with the fifth text");
        assert_eq!(*rig.replies.lock().unwrap(), ["1", "5"], "the texts that were replaced are never answered");
    }

    #[test]
    fn a_save_is_sent_after_the_pending_text() {
        let rig = rig();
        rig.sender.push_latest("a", changed("1"), rig.done("1"));
        rig.first_send_in_flight();
        rig.sender.push_latest("a", changed("2"), rig.done("2"));
        rig.sender.push("a", saved(), rig.done("saved"));
        // A text after the save is not folded into the one before it.
        rig.sender.push_latest("a", changed("3"), rig.done("3"));
        assert_eq!(rig.finish(), ["changed 1", "changed 2", "saved", "changed 3"]);
    }

    #[test]
    fn close_after_pending_text_sends_both_in_order() {
        let rig = rig();
        rig.sender.push_latest("a", changed("1"), rig.done("1"));
        rig.first_send_in_flight();
        rig.sender.push_latest("a", changed("2"), rig.done("2"));
        rig.sender.push("a", closed(), rig.done("closed"));
        assert_eq!(rig.finish(), ["changed 1", "changed 2", "closed"]);
    }

    /// Files do not wait for each other's texts, and coalescing is per file.
    #[test]
    fn texts_of_different_files_are_not_folded_together() {
        let rig = rig();
        rig.sender.push_latest("a", changed("1"), rig.done("a1"));
        rig.first_send_in_flight();
        rig.sender.push_latest("b", changed("b-text"), rig.done("b"));
        rig.sender.push_latest("a", changed("2"), rig.done("a2"));
        rig.sender.push_latest("a", changed("3"), rig.done("a3"));
        assert_eq!(rig.finish(), ["changed 1", "changed b-text", "changed 3"]);
    }

    // ── conflicts, from any line ────────────────────────────────────────────

    #[test]
    fn conflict_actions_are_offered_on_any_line() {
        let at = |line: u32| Range {
            start: Position { line, character: 0 },
            end: Position { line, character: 0 },
        };
        for line in [0, 1, 7, 400] {
            let actions = conflict_offer(&at(line), true, &uri(), "content/post/a.md", 3, "mine\n", "disk\n");
            assert_eq!(
                actions.iter().map(title).collect::<Vec<_>>(),
                [TAKE_FILE, KEEP_EDITS],
                "line {line}"
            );
        }
        // Not conflicted: nothing, wherever the cursor is.
        assert!(conflict_offer(&at(0), false, &uri(), "content/post/a.md", 3, "mine\n", "disk\n").is_empty());
    }

    // ── drift and held regions ──────────────────────────────────────────────

    fn drifted() -> Synced {
        Synced {
            version: 7,
            text: "one\n\nmine here\n\nthree\n".to_string(),
            drifts: vec![Drift {
                id: DriftId(0xab),
                kind: DriftKind::Blocks,
                path: vec![],
                range: 5..14,
                current: "theirs".to_string(),
            }],
            held: vec![],
        }
    }

    #[test]
    fn drift_becomes_a_diagnostic_with_two_actions() {
        let synced = drifted();
        let diagnostics = sync_diagnostics(&synced);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
        assert_eq!(
            diagnostics[0].range,
            Range {
                start: Position { line: 2, character: 0 },
                end: Position { line: 2, character: 9 }
            }
        );

        let actions = sync_actions(&uri(), &synced, &diagnostics[0].range);
        assert_eq!(actions.iter().map(title).collect::<Vec<_>>(), [TAKE_CURRENT, KEEP_MINE_TITLE]);
        let keep = command_of(&actions[1]);
        assert_eq!(keep.command, KEEP_MINE);
        assert_eq!(
            keep.arguments.as_deref(),
            Some(&[json!(uri().to_string()), json!("00000000000000ab")][..])
        );
        assert_eq!(parse_drift_id("00000000000000ab"), Some(DriftId(0xab)));

        // Away from the drift there is nothing to resolve.
        let elsewhere = Range { start: Position { line: 0, character: 0 }, end: Position { line: 0, character: 3 } };
        assert!(sync_actions(&uri(), &synced, &elsewhere).is_empty());
    }

    #[test]
    fn a_list_spacing_drift_says_what_it_is_about() {
        let mut synced = drifted();
        synced.drifts[0].kind = DriftKind::ListSpacing;
        let diagnostics = sync_diagnostics(&synced);
        assert_eq!(
            diagnostics[0].message,
            "This list's spacing changed here while one of its items was changed elsewhere."
        );
        // The same two ways out.
        let actions = sync_actions(&uri(), &synced, &diagnostics[0].range);
        assert_eq!(actions.iter().map(title).collect::<Vec<_>>(), [TAKE_CURRENT, KEEP_MINE_TITLE]);
    }

    #[test]
    fn held_region_becomes_an_information_diagnostic() {
        let synced = Synced {
            version: 1,
            text: "a\n\n```rust\nlet x = 1;\n".to_string(),
            drifts: vec![],
            held: vec![HeldRegion {
                path: vec![],
                range: 3..21,
                reason: "unterminated fenced code block".to_string(),
            }],
        };
        let diagnostics = sync_diagnostics(&synced);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::INFORMATION));
        assert_eq!(
            diagnostics[0].message,
            "This code fence isn't closed yet; the blocks after it are unchanged until you close it."
        );
        assert_eq!(diagnostics[0].range.start, Position { line: 2, character: 0 });
        assert!(sync_actions(&uri(), &synced, &diagnostics[0].range).is_empty(), "nothing to resolve");

        // An HTML block says so.
        assert_eq!(
            held_message("unterminated HTML block (comment)"),
            "This HTML block isn't closed yet; the blocks after it are unchanged until you close it."
        );
    }

    #[test]
    fn take_current_version_edits_only_that_range() {
        // A four-byte, two-unit character before the range: UTF-16 positions.
        let synced = Synced {
            version: 7,
            text: "\u{1F600} mine\n".to_string(),
            drifts: vec![Drift {
                id: DriftId(1),
                kind: DriftKind::Blocks,
                path: vec![],
                range: 5..9,
                current: "theirs".to_string(),
            }],
            held: vec![],
        };
        let request = Range {
            start: Position { line: 0, character: 4 },
            end: Position { line: 0, character: 4 },
        };
        let actions = sync_actions(&uri(), &synced, &request);
        let (version, edits) = edits_of(&actions[0]);
        assert_eq!(version, Some(7), "a versioned edit: refused if the buffer moved on");
        assert_eq!(
            edits,
            vec![TextEdit {
                range: Range {
                    start: Position { line: 0, character: 3 },
                    end: Position { line: 0, character: 7 }
                },
                new_text: "theirs".to_string(),
            }]
        );
        assert_eq!(apply(&synced.text, &edits), "\u{1F600} theirs\n");
    }

    // ── conflicts ───────────────────────────────────────────────────────────

    #[test]
    fn conflict_actions_use_the_neutral_wording() {
        let message = conflict_diagnostic().message;
        for word in ["browser", "REPL", "presemble reload"] {
            assert!(!message.contains(word), "{word}: {message}");
        }
        let actions = conflict_actions(&uri(), "content/post/a.md", 3, "mine\n", "disk\n");
        assert_eq!(
            actions.iter().map(title).collect::<Vec<_>>(),
            ["Take the file on disk (discard unsaved edits)", "Keep the unsaved edits (overwrite the file)"]
        );
        assert_eq!(command_of(&actions[0]).command, RELOAD_DOCUMENT);
        assert_eq!(command_of(&actions[1]).command, OVERWRITE_FILE);
        assert_eq!(command_of(&actions[1]).arguments.as_deref(), Some(&[json!("content/post/a.md")][..]));
    }

    #[test]
    fn take_the_file_on_disk_replaces_the_buffer_text() {
        let (buffer, file) = ("one\n\nmine\n", "one\n\ndisk\n\nmore\n");
        let actions = conflict_actions(&uri(), "content/post/a.md", 3, buffer, file);
        let (version, edits) = edits_of(&actions[0]);
        assert_eq!(version, Some(3), "a versioned edit");
        assert_eq!(apply(buffer, &edits), file, "the buffer is the file's text");
        // The reload runs too.
        assert_eq!(command_of(&actions[0]).command, RELOAD_DOCUMENT);
    }

    // ── spelling ────────────────────────────────────────────────────────────

    #[test]
    fn match_file_is_offered_only_for_the_same_content() {
        // The same content, spelled differently.
        assert!(match_file_edits("* a\n* b\n", "- a\n- b\n").is_some());
        // The same text: nothing to match.
        assert!(match_file_edits("- a\n", "- a\n").is_none());
        // Other content: not a matter of spelling.
        assert!(match_file_edits("- a\n", "- b\n").is_none());
        assert!(match_file_edits("one\n\ntwo\n", "one\n").is_none());
    }

    #[test]
    fn match_file_makes_the_buffer_the_file_text() {
        let (buffer, file) = ("* a\n* b\n\nOne  *two*\n", "- a\n- b\n\nOne  _two_\n");
        let edits = match_file_edits(buffer, file).expect("offered");
        assert_eq!(apply(buffer, &edits), file);
    }

    fn grammar() -> schema::Grammar {
        schema::parse_schema("# Post title {#title}\noccurs\n: exactly once\n\n----\nBody.\n").expect("a schema")
    }

    #[test]
    fn formatting_canonicalises_the_buffer() {
        let src = "# Title\n\n----\n\n* a\n* b\n";
        let edits = formatting_edits(src, &grammar());
        assert!(!edits.is_empty());
        let formatted = apply(src, &edits);
        assert!(formatted.contains("- a\n- b"), "{formatted:?}");
        assert!(!formatted.contains('*'), "{formatted:?}");
        // Same content, now canonical.
        assert!(content::sync::changed_block_edits(src, &formatted).is_empty());
    }

    #[test]
    fn formatting_of_canonical_text_is_no_edit() {
        let src = "# Title\n\n----\n\n* a\n* b\n";
        let canonical = apply(src, &formatting_edits(src, &grammar()));
        assert!(formatting_edits(&canonical, &grammar()).is_empty());
    }
}
