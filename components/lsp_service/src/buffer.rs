//! The pure parts of the language server's editor sync (LSP-2, 2.5): what is
//! sent to the conductor and when, and what the conductor's answers become in
//! the editor (diagnostics and code actions). Nothing here is tied to a real
//! conductor or to the client: the line to the conductor is a [`Link`] that
//! the server supplies (and a test replaces with a slow fake).
//!
//! Positions are LSP positions: lines, and UTF-16 code units in a line.

use conductor::{
    ConflictId, ConflictRegion, ConflictSide, ConflictState, Drift, DriftId, DriftKind, HeldRegion,
};
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
        let shared = std::sync::Arc::new(Shared {
            state: Default::default(),
            wake: Default::default(),
        });
        let worker = shared.clone();
        std::thread::spawn(move || {
            loop {
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
            }
        });
        Sender { shared }
    }

    /// Queue a job behind everything queued so far.
    pub fn push(&self, key: &str, cmd: conductor::Command, done: Done) {
        self.queue(Job {
            key: key.to_string(),
            latest_wins: false,
            cmd,
            done,
        });
    }

    /// Queue a text of a file: it replaces the newest job of that file if that
    /// is a text that still waits.
    pub fn push_latest(&self, key: &str, cmd: conductor::Command, done: Done) {
        self.queue(Job {
            key: key.to_string(),
            latest_wins: true,
            cmd,
            done,
        });
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
            state = self
                .shared
                .wake
                .wait(state)
                .unwrap_or_else(|e| e.into_inner());
        }
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.shared.lock().stopped = true;
        self.shared.wake.notify_all();
    }
}

/// What an editor knows of its document: the buffer's text and, when it
/// parses, the document read from it (to find blocks by their location).
pub struct BufferDoc<'a> {
    pub text: &'a str,
    pub doc: Option<&'a content::Document>,
}

/// The conflict actions to offer for a code action request, given the
/// document's conflict with the file on disk (`None`: not in conflict). A
/// whole-file conflict offers its two ways out wherever the cursor is; a block
/// conflict offers its two on its block.
pub fn conflict_offer(
    request: &Range,
    conflict: Option<&ConflictState>,
    uri: &Url,
    rel: &str,
    version: i32,
    buffer: &BufferDoc,
    file: &str,
) -> Vec<CodeActionOrCommand> {
    match conflict {
        None | Some(ConflictState::Resolved) => Vec::new(),
        Some(ConflictState::WholeFile) => conflict_actions(uri, rel, version, buffer.text, file),
        Some(ConflictState::Blocks(regions)) => regions
            .iter()
            .filter_map(|region| {
                let range = region_range(buffer, region);
                overlaps(&range, request).then(|| region_actions(uri, rel, version, range, region))
            })
            .flatten()
            .collect(),
    }
}

/// The diagnostics for the document's conflict with the file on disk
/// (`None` or `Resolved`: none). A whole-file conflict is one warning on the
/// first line; a block conflict is a warning on its block in the buffer.
pub fn conflict_diagnostics(
    conflict: Option<&ConflictState>,
    buffer: &BufferDoc,
) -> Vec<Diagnostic> {
    match conflict {
        None | Some(ConflictState::Resolved) => Vec::new(),
        Some(ConflictState::WholeFile) => vec![conflict_diagnostic()],
        Some(ConflictState::Blocks(regions)) => regions
            .iter()
            .map(|region| Diagnostic {
                range: region_range(buffer, region),
                severity: Some(DiagnosticSeverity::WARNING),
                source: Some("presemble".to_string()),
                message: "This block changed on disk differently from the unsaved edits. Take the file's version of this block, or keep the unsaved version."
                    .to_string(),
                ..Default::default()
            })
            .collect(),
    }
}

/// Where a conflict's block is in the buffer: the block its location (in N)
/// names in the buffer's document, or the nearest one on its path, or the
/// first line.
pub fn region_range(buffer: &BufferDoc, region: &ConflictRegion) -> Range {
    let (Some(doc), Some(location)) = (buffer.doc, &region.location) else {
        return Range::default();
    };
    let mut location = content::Location::from(location);
    loop {
        if let Some(span) = content::span_of_location(doc, &location)
            && let Some(slice) = buffer.text.get(span.start..span.end)
        {
            // Without the line break the parser leaves behind a block (the
            // region's text, like the file's version of it, is trimmed).
            let start = span.start + (slice.len() - slice.trim_start().len());
            return lsp_range(buffer.text, &(start..start + slice.trim().len()));
        }
        if location.path.pop().is_none() {
            return Range::default();
        }
    }
}

/// The buffer edit that comes with taking the file's version of a block: its
/// range in the buffer becomes the file's text of it. Sent as the last
/// argument of the command, and applied once the conductor has taken it.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEdit {
    pub uri: Url,
    pub version: i32,
    pub range: Range,
    pub text: String,
}

impl BlockEdit {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "uri": self.uri.to_string(),
            "version": self.version,
            "range": serde_json::to_value(self.range).unwrap_or_default(),
            "text": self.text,
        })
    }

    fn from_json(value: &serde_json::Value) -> Option<BlockEdit> {
        Some(BlockEdit {
            uri: Url::parse(value.get("uri")?.as_str()?).ok()?,
            version: i32::try_from(value.get("version")?.as_i64()?).ok()?,
            range: serde_json::from_value(value.get("range")?.clone()).ok()?,
            text: value.get("text")?.as_str()?.to_string(),
        })
    }
}

/// The two ways out of a block conflict: each resolves that block alone, in
/// the conductor (`rel` is the site-relative path of the file). Taking the
/// file's version also carries the edit of the block in the buffer.
fn region_actions(
    uri: &Url,
    rel: &str,
    version: i32,
    range: Range,
    region: &ConflictRegion,
) -> Vec<CodeActionOrCommand> {
    let edit = BlockEdit {
        uri: uri.clone(),
        version,
        range,
        text: region.disk.clone(),
    };
    [
        (TAKE_FILE_BLOCK, SIDE_DISK, Some(edit)),
        (KEEP_UNSAVED_BLOCK, SIDE_UNSAVED, None),
    ]
    .into_iter()
    .map(|(title, side, edit)| {
        let mut arguments = vec![json!(rel), json!(region.id.to_string()), json!(side)];
        if let Some(edit) = edit {
            arguments.push(edit.to_json());
        }
        CodeActionOrCommand::CodeAction(CodeAction {
            title: title.to_string(),
            kind: Some(CodeActionKind::QUICKFIX),
            command: Some(command(title, RESOLVE_CONFLICT, arguments)),
            ..Default::default()
        })
    })
    .collect()
}

/// A block resolution read from the arguments of a [`RESOLVE_CONFLICT`]
/// command: the site-relative file, the command for the conductor, and the
/// buffer edit to apply once it has been taken (taking the file's version).
pub struct Resolution {
    pub rel: String,
    pub command: conductor::Command,
    pub edit: Option<BlockEdit>,
}

/// The resolution the arguments of a [`RESOLVE_CONFLICT`] command spell.
pub fn parse_resolution(arguments: &[serde_json::Value]) -> Option<Resolution> {
    let text = |i: usize| arguments.get(i).and_then(|v| v.as_str());
    let rel = text(0)?.to_string();
    let path = conductor::editorial_types::ContentPath::parse(&rel).ok()?;
    let id = parse_conflict_id(text(1)?)?;
    let take = match text(2)? {
        SIDE_DISK => ConflictSide::Disk,
        SIDE_UNSAVED => ConflictSide::Unsaved,
        _ => return None,
    };
    let edit = arguments.get(3).and_then(BlockEdit::from_json);
    Some(Resolution {
        rel,
        command: conductor::Command::ResolveConflict { path, id, take },
        edit,
    })
}

/// Queue `resolution` for the conductor; when it has answered, `then` gets
/// the buffer edit to apply: the block's file version, only once the
/// conductor has taken it (so N holds that text when the edit reaches it as a
/// change, and it lowers to nothing). `None` when there is nothing to apply.
pub fn resolve_conflict(
    sender: &Sender,
    key: &str,
    resolution: Resolution,
    then: impl FnOnce(Option<WorkspaceEdit>) + Send + 'static,
) {
    let edit = resolution.edit;
    sender.push(
        key,
        resolution.command,
        Box::new(move |reply| {
            let taken = matches!(reply, Ok(conductor::Response::Ok));
            then(edit.filter(|_| taken).map(|edit| {
                versioned_edit(
                    &edit.uri,
                    edit.version,
                    vec![TextEdit {
                        range: edit.range,
                        new_text: edit.text,
                    }],
                )
            }))
        }),
    );
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
pub const RESOLVE_CONFLICT: &str = "presemble.resolveConflict";

/// The titles of the code actions.
pub const TAKE_CURRENT: &str = "Take the current version";
pub const KEEP_MINE_TITLE: &str = "Keep mine";
pub const TAKE_FILE: &str = "Take the file on disk (discard unsaved edits)";
pub const KEEP_EDITS: &str = "Keep the unsaved edits (overwrite the file)";
pub const MATCH_FILE: &str = "Match the file on disk";
pub const TAKE_FILE_BLOCK: &str = "Take the file's version of this block";
pub const KEEP_UNSAVED_BLOCK: &str = "Keep the unsaved version";

/// The side a [`RESOLVE_CONFLICT`] command takes, as spelled in its arguments.
pub const SIDE_DISK: &str = "disk";
pub const SIDE_UNSAVED: &str = "unsaved";

/// The LSP range of the bytes `range` of `text`.
pub fn lsp_range(text: &str, range: &std::ops::Range<usize>) -> Range {
    let (start_line, start_character) = content::byte_to_position(text, range.start);
    let (end_line, end_character) = content::byte_to_position(text, range.end);
    Range {
        start: Position {
            line: start_line,
            character: start_character,
        },
        end: Position {
            line: end_line,
            character: end_character,
        },
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
    let what = if reason.contains("fence") {
        "code fence"
    } else {
        "HTML block"
    };
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
            text_document: OptionalVersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: Some(version),
            },
            edits: edits.into_iter().map(OneOf::Left).collect(),
        }])),
        ..Default::default()
    }
}

fn command(title: &str, command: &str, arguments: Vec<serde_json::Value>) -> Command {
    Command {
        title: title.to_string(),
        command: command.to_string(),
        arguments: Some(arguments),
    }
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
                vec![TextEdit {
                    range,
                    new_text: drift.current.clone(),
                }],
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

/// The conflict id spelled in a command argument (`ConflictId`'s `Display`).
pub fn parse_conflict_id(spelled: &str) -> Option<ConflictId> {
    spelled.parse().ok()
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
        .map(|e| TextEdit {
            range: lsp_range(text, &e.range),
            new_text: e.text.clone(),
        })
        .collect()
}

/// The edits that make `buffer` the text of `file`, when the two read as the
/// same content but are spelled differently (`* a` and `- a`). `None` when the
/// texts are the same, or when the content differs: then it is not a matter of
/// spelling and there is nothing to match.
pub fn match_file_edits(buffer: &str, file: &str) -> Option<Vec<TextEdit>> {
    if buffer == file || !content::sync::changed_block_edits(buffer, file).is_empty() {
        return None;
    }
    Some(text_edits(
        buffer,
        &content::sync::format_edits(buffer, file),
    ))
}

/// The edits that make `src` canonical: the text N would write for it. Empty
/// when it is canonical already, or cannot be read.
pub fn formatting_edits(src: &str, grammar: &schema::Grammar) -> Vec<TextEdit> {
    let Ok(doc) = content::parse_and_assign(src, grammar) else {
        return Vec::new();
    };
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
                line_start += text[line_start..]
                    .find('\n')
                    .map_or(text.len() - line_start, |i| i + 1);
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
        let CodeActionOrCommand::CodeAction(action) = action else {
            panic!("a code action")
        };
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
            path: conductor::editorial_types::ContentPath::parse("content/post/a.md")
                .expect("a content path"),
            text: text.to_string(),
        }
    }

    fn closed() -> conductor::Command {
        conductor::Command::EditorClosed {
            path: conductor::editorial_types::ContentPath::parse("content/post/a.md")
                .expect("a content path"),
        }
    }

    fn saved() -> conductor::Command {
        conductor::Command::DocumentSaved {
            path: conductor::editorial_types::SitePath::parse("content/post/a.md")
                .expect("a site path"),
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
        let sender = Sender::start(Slow {
            log: log.clone(),
            started: started_tx,
            release: release_rx,
            first: true,
        });
        Rig {
            sender,
            log,
            replies: Arc::new(Mutex::new(Vec::new())),
            started,
            release,
        }
    }

    impl Rig {
        fn done(&self, name: &str) -> Done {
            let replies = self.replies.clone();
            let name = name.to_string();
            Box::new(move |_| replies.lock().unwrap().push(name))
        }

        /// Wait until the first send is in flight.
        fn first_send_in_flight(&self) {
            self.started
                .recv_timeout(Duration::from_secs(2))
                .expect("the first send began");
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
            rig.sender
                .push_latest("a", changed(&n.to_string()), rig.done(&n.to_string()));
        }
        assert_eq!(
            rig.finish(),
            ["changed 1", "changed 5"],
            "exactly two sends, the second with the fifth text"
        );
        assert_eq!(
            *rig.replies.lock().unwrap(),
            ["1", "5"],
            "the texts that were replaced are never answered"
        );
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
        assert_eq!(
            rig.finish(),
            ["changed 1", "changed 2", "saved", "changed 3"]
        );
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
        rig.sender
            .push_latest("b", changed("b-text"), rig.done("b"));
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
        let buffer = BufferDoc {
            text: "mine\n",
            doc: None,
        };
        for line in [0, 1, 7, 400] {
            let actions = conflict_offer(
                &at(line),
                Some(&ConflictState::WholeFile),
                &uri(),
                "content/post/a.md",
                3,
                &buffer,
                "disk\n",
            );
            assert_eq!(
                actions.iter().map(title).collect::<Vec<_>>(),
                [TAKE_FILE, KEEP_EDITS],
                "line {line}"
            );
        }
        // Not conflicted: nothing, wherever the cursor is.
        assert!(
            conflict_offer(
                &at(0),
                None,
                &uri(),
                "content/post/a.md",
                3,
                &buffer,
                "disk\n"
            )
            .is_empty()
        );
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
                start: Position {
                    line: 2,
                    character: 0
                },
                end: Position {
                    line: 2,
                    character: 9
                }
            }
        );

        let actions = sync_actions(&uri(), &synced, &diagnostics[0].range);
        assert_eq!(
            actions.iter().map(title).collect::<Vec<_>>(),
            [TAKE_CURRENT, KEEP_MINE_TITLE]
        );
        let keep = command_of(&actions[1]);
        assert_eq!(keep.command, KEEP_MINE);
        assert_eq!(
            keep.arguments.as_deref(),
            Some(&[json!(uri().to_string()), json!("00000000000000ab")][..])
        );
        assert_eq!(parse_drift_id("00000000000000ab"), Some(DriftId(0xab)));

        // Away from the drift there is nothing to resolve.
        let elsewhere = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: 0,
                character: 3,
            },
        };
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
        assert_eq!(
            actions.iter().map(title).collect::<Vec<_>>(),
            [TAKE_CURRENT, KEEP_MINE_TITLE]
        );
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
        assert_eq!(
            diagnostics[0].severity,
            Some(DiagnosticSeverity::INFORMATION)
        );
        assert_eq!(
            diagnostics[0].message,
            "This code fence isn't closed yet; the blocks after it are unchanged until you close it."
        );
        assert_eq!(
            diagnostics[0].range.start,
            Position {
                line: 2,
                character: 0
            }
        );
        assert!(
            sync_actions(&uri(), &synced, &diagnostics[0].range).is_empty(),
            "nothing to resolve"
        );

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
            start: Position {
                line: 0,
                character: 4,
            },
            end: Position {
                line: 0,
                character: 4,
            },
        };
        let actions = sync_actions(&uri(), &synced, &request);
        let (version, edits) = edits_of(&actions[0]);
        assert_eq!(
            version,
            Some(7),
            "a versioned edit: refused if the buffer moved on"
        );
        assert_eq!(
            edits,
            vec![TextEdit {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 3
                    },
                    end: Position {
                        line: 0,
                        character: 7
                    }
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
            [
                "Take the file on disk (discard unsaved edits)",
                "Keep the unsaved edits (overwrite the file)"
            ]
        );
        assert_eq!(command_of(&actions[0]).command, RELOAD_DOCUMENT);
        assert_eq!(command_of(&actions[1]).command, OVERWRITE_FILE);
        assert_eq!(
            command_of(&actions[1]).arguments.as_deref(),
            Some(&[json!("content/post/a.md")][..])
        );
    }

    /// A block conflict of `file` on the body's block `index` (in N): the
    /// paragraph "Beta disk.".
    fn region(file: &str, index: usize) -> ConflictRegion {
        let start = file.find("Beta disk.").expect("the block");
        ConflictRegion {
            id: ConflictId(0xab),
            path: Vec::new(),
            location: Some(conductor::BlockLocation {
                container: conductor::BlockContainer::Body,
                index,
                path: Vec::new(),
            }),
            disk_range: start..start + "Beta disk.".len(),
            disk: "Beta disk.".to_string(),
        }
    }

    /// The document of a buffer, read with the post grammar.
    fn read(buffer: &str) -> content::Document {
        content::parse_and_assign(buffer, &grammar()).expect("the buffer parses")
    }

    fn blocks(regions: Vec<ConflictRegion>) -> ConflictState {
        ConflictState::Blocks(regions)
    }

    #[test]
    fn a_conflict_region_becomes_a_diagnostic_with_two_actions() {
        let file = "# Title\n\n----\n\nAlpha.\n\nBeta disk.\n";
        let buffer = "# Title\n\n----\n\nAlpha.\n\nBeta mine.\n";
        let doc = read(buffer);
        let buffer = BufferDoc {
            text: buffer,
            doc: Some(&doc),
        };
        let state = blocks(vec![region(file, 1)]);
        let regions = Some(&state);
        let diagnostics = conflict_diagnostics(regions, &buffer);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let at = Range {
            start: Position {
                line: 6,
                character: 0,
            },
            end: Position {
                line: 6,
                character: 10,
            },
        };
        assert_eq!(diagnostics[0].range, at);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));

        // Two ways out, on the block.
        let actions = conflict_offer(&at, regions, &uri(), "content/post/a.md", 3, &buffer, file);
        assert_eq!(
            actions.iter().map(title).collect::<Vec<_>>(),
            [TAKE_FILE_BLOCK, KEEP_UNSAVED_BLOCK]
        );
        for (action, side) in actions.iter().zip([SIDE_DISK, SIDE_UNSAVED]) {
            let command = command_of(action);
            assert_eq!(command.command, RESOLVE_CONFLICT);
            let arguments = command.arguments.clone().unwrap_or_default();
            assert_eq!(
                arguments[..3],
                [
                    json!("content/post/a.md"),
                    json!("00000000000000ab"),
                    json!(side)
                ]
            );
        }
        assert_eq!(
            parse_conflict_id("00000000000000ab"),
            Some(ConflictId(0xab))
        );

        // Away from the block: nothing.
        let elsewhere = Range::default();
        assert!(
            conflict_offer(
                &elsewhere,
                regions,
                &uri(),
                "content/post/a.md",
                3,
                &buffer,
                file
            )
            .is_empty()
        );
    }

    /// The buffer has a paragraph above the conflicted block that the file
    /// does not: the conflict still lands on its own block in the buffer, not
    /// where the block is in the file.
    #[test]
    fn a_conflict_lands_on_its_block_in_the_buffer_when_blocks_above_differ() {
        let file = "# Title\n\n----\n\nAlpha.\n\nBeta disk.\n";
        let text = "# Title\n\n----\n\nAlpha.\n\nAdded above.\n\nBeta mine.\n";
        let doc = read(text);
        let buffer = BufferDoc {
            text,
            doc: Some(&doc),
        };
        let state = blocks(vec![region(file, 2)]);
        let diagnostics = conflict_diagnostics(Some(&state), &buffer);
        assert_eq!(
            diagnostics.iter().map(|d| d.range).collect::<Vec<_>>(),
            [Range {
                start: Position {
                    line: 8,
                    character: 0
                },
                end: Position {
                    line: 8,
                    character: 10
                },
            }],
            "exactly the block \"Beta mine.\""
        );
    }

    /// A location the buffer has no block for (it no longer parses into one):
    /// the nearest block on its path, or else the first line.
    #[test]
    fn a_conflict_without_its_block_in_the_buffer_falls_back() {
        let file = "# Title\n\n----\n\n> Alpha.\n>\n> Beta disk.\n";
        let text = "# Title\n\n----\n\n> Alpha.\n";
        let doc = read(text);
        let buffer = BufferDoc {
            text,
            doc: Some(&doc),
        };
        let mut nested = region(file, 0);
        nested.location = Some(conductor::BlockLocation {
            container: conductor::BlockContainer::Body,
            index: 0,
            path: vec![conductor::BlockStep::Block(1)],
        });
        // The quote, the nearest block on the way.
        assert_eq!(
            region_range(&buffer, &nested).start,
            Position {
                line: 4,
                character: 0
            }
        );
        // No document (the buffer does not parse), or no location: line 0.
        let unparsed = BufferDoc { text, doc: None };
        assert_eq!(region_range(&unparsed, &nested), Range::default());
        nested.location = None;
        assert_eq!(region_range(&buffer, &nested), Range::default());
    }

    /// GUARD: a whole-document conflict keeps the line-0 diagnostic and its two
    /// ways out (reload, or save with consent).
    #[test]
    fn a_whole_document_conflict_keeps_its_actions() {
        let buffer = BufferDoc {
            text: "mine\n",
            doc: None,
        };
        let diagnostics = conflict_diagnostics(Some(&ConflictState::WholeFile), &buffer);
        assert_eq!(diagnostics, vec![conflict_diagnostic()]);
        let actions = conflict_offer(
            &Range::default(),
            Some(&ConflictState::WholeFile),
            &uri(),
            "content/post/a.md",
            3,
            &buffer,
            "disk\n",
        );
        assert_eq!(
            actions.iter().map(title).collect::<Vec<_>>(),
            [TAKE_FILE, KEEP_EDITS]
        );
        assert!(conflict_diagnostics(None, &buffer).is_empty());
    }

    /// A resolved conflict leaves no diagnostics and no actions behind.
    #[test]
    fn resolved_clears_the_conflict_diagnostics() {
        let buffer = BufferDoc {
            text: "mine\n",
            doc: None,
        };
        assert!(conflict_diagnostics(Some(&ConflictState::Resolved), &buffer).is_empty());
        assert!(
            conflict_offer(
                &Range::default(),
                Some(&ConflictState::Resolved),
                &uri(),
                "content/post/a.md",
                3,
                &buffer,
                "disk\n",
            )
            .is_empty()
        );
    }

    /// A conductor that answers at once and logs what it is sent.
    struct Logged(Arc<Mutex<Vec<String>>>);

    impl Link for Logged {
        fn send(&mut self, cmd: &conductor::Command) -> Result<conductor::Response, String> {
            self.0.lock().unwrap().push(format!("{cmd:?}"));
            Ok(conductor::Response::Ok)
        }
    }

    /// Taking the file's version of a block: the conductor is told first, and
    /// once it has answered, the block in the buffer becomes the file's text.
    #[test]
    fn taking_the_files_version_also_replaces_the_block_in_the_buffer() {
        let file = "# Title\n\n----\n\nAlpha.\n\nBeta disk.\n";
        let text = "# Title\n\n----\n\nAlpha.\n\nAdded above.\n\nBeta mine.\n";
        let doc = read(text);
        let buffer = BufferDoc {
            text,
            doc: Some(&doc),
        };
        let state = blocks(vec![region(file, 2)]);
        let at = Range {
            start: Position {
                line: 8,
                character: 2,
            },
            end: Position {
                line: 8,
                character: 2,
            },
        };
        let actions = conflict_offer(
            &at,
            Some(&state),
            &uri(),
            "content/post/a.md",
            3,
            &buffer,
            file,
        );
        let take = command_of(&actions[0]);
        let resolution =
            parse_resolution(take.arguments.as_deref().unwrap_or_default()).expect("a resolution");

        let log = Arc::new(Mutex::new(Vec::new()));
        let sender = Sender::start(Logged(log.clone()));
        let (tx, rx) = mpsc::channel();
        let seen = log.clone();
        resolve_conflict(&sender, "a", resolution, move |edit| {
            // The conductor had been told when the edit comes.
            tx.send((seen.lock().unwrap().clone(), edit)).unwrap();
        });
        let (sent, edit) = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the resolution finishes");
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(
            sent[0].starts_with("ResolveConflict") && sent[0].contains("Disk"),
            "{sent:?}"
        );
        let edit = edit.expect("a buffer edit");
        let changes = edit.document_changes.expect("versioned");
        let DocumentChanges::Edits(edits) = changes else {
            panic!("text edits");
        };
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].text_document.version, Some(3));
        let edits: Vec<TextEdit> = edits[0]
            .edits
            .iter()
            .map(|e| match e {
                OneOf::Left(e) => e.clone(),
                OneOf::Right(e) => e.text_edit.clone(),
            })
            .collect();
        assert_eq!(
            edits,
            [TextEdit {
                range: Range {
                    start: Position {
                        line: 8,
                        character: 0
                    },
                    end: Position {
                        line: 8,
                        character: 10
                    },
                },
                new_text: "Beta disk.".to_string(),
            }]
        );
    }

    /// Keeping the unsaved version only tells the conductor.
    #[test]
    fn keeping_the_unsaved_version_leaves_the_buffer() {
        let file = "# Title\n\n----\n\nAlpha.\n\nBeta disk.\n";
        let text = "# Title\n\n----\n\nAlpha.\n\nBeta mine.\n";
        let doc = read(text);
        let buffer = BufferDoc {
            text,
            doc: Some(&doc),
        };
        let state = blocks(vec![region(file, 1)]);
        let actions = conflict_offer(
            &Range {
                start: Position {
                    line: 6,
                    character: 0,
                },
                end: Position {
                    line: 6,
                    character: 0,
                },
            },
            Some(&state),
            &uri(),
            "content/post/a.md",
            3,
            &buffer,
            file,
        );
        let keep = command_of(&actions[1]);
        let resolution =
            parse_resolution(keep.arguments.as_deref().unwrap_or_default()).expect("a resolution");
        let log = Arc::new(Mutex::new(Vec::new()));
        let sender = Sender::start(Logged(log.clone()));
        let (tx, rx) = mpsc::channel();
        resolve_conflict(&sender, "a", resolution, move |edit| {
            tx.send(edit).unwrap();
        });
        let edit = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the resolution finishes");
        assert!(edit.is_none());
        let sent = log.lock().unwrap().clone();
        assert!(sent[0].contains("Unsaved"), "{sent:?}");
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
        schema::parse_schema("# Post title {#title}\noccurs\n: exactly once\n\n----\nBody.\n")
            .expect("a schema")
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
