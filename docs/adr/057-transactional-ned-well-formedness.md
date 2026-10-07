# ADR-057: NED edits are typed operations applied select-then-apply; well-formedness is enforced, schema validity is reported

## Status

Accepted

Rewritten in stage 2e-2. The first version of this ADR (journaled transactions: clone the store, journal, check, swap) was not implemented; the typed-operations design below replaced it and is implemented. What it leaves open is listed under "Channel gaps still open" and "Open questions".

## Context

ADR-056 made suggestion accept apply structured mutations under one write lock, but left open how an edit is checked and how it fails. Before this rewrite the node store had no notion of an edit that could be refused whole: a mutation could be half applied, raw `/_presemble/apply` programs could leave arbitrary nodes behind, and each channel (browser, REPL, MCP, suggestion accept) had its own mutation path and its own errors.

Principles:

- **Every channel is a codec over N.** Edits enter as NED and go through one executor with one error model. The browser, the REPL, MCP and files are representations of N, not separate edit paths.
- **Schema-invalid content is legitimate state** (2026-10-01). It can be edited, held and saved; schema validity decides only whether it can be published (ADR-061). The only input that can fail to parse is data crossing a boundary: files on disk, suggestion payloads, wire programs. Nodes inside the store are already parsed.
- **Select, then apply.** A program names what it changes first; nothing is written until every selection is resolved and checked. There is no clone-and-swap transaction.

## Decision

### 1. Typed operations

`content_store::edit` has three steps:

- **`resolve`** turns a selection into exactly one target of the kind the mutation needs (`Target::Text`, `Target::Block` or `Target::Container`) inside a live document. Zero matches, several matches, a wrong kind, or a node outside a live document is an `AddressError`.
- **`prepare`** checks a mutation against that target and returns a `PreparedEdit`. It does not touch the store.

`resolve` and `prepare` together enforce:

- exactly one target, in a live document, of the right kind;
- no change to a document frame (`document`, `preamble`, `body`), because creating or deleting a page is a lifecycle operation with file-system effects and has its own command;
- the payload holds no `Existing` node;
- the payload holds only payload block kinds;
- the postcondition: the edited block round-trips through markdown, so it can be written to disk.
- **`commit`** writes prepared edits in order. It cannot fail on input: positions are looked up at commit time, and a missing anchor or a gone target is a broken invariant (a bug), not input. It writes the canonical encoding.

`EditError` names the refusals of `prepare`: target mismatch, `Existing` node, empty payload, a payload or owning block that does not decode, a failed postcondition, empty or missing or non-unique search text, and `NoChange`.

This replaces the first version's scope rules and well-formedness gate. "Can this be written to disk?" is answered by the postcondition at `prepare`, before anything is written. It never asks "does this match the schema?".

### 2. Select-then-apply

- **`Plan` and `execute`.** A `Plan` is a list of `PlannedMutation { targets, mutation, origin }`. `execute` runs it under one write lock:
  1. resolve every step;
  2. prepare every step against the unchanged store;
  3. check that no two steps' footprints overlap (the same node, or one inside the other);
  4. commit all steps in plan order.

  Any failure leaves the store untouched and reports the first failing step. The result is all of the plan or none of it.
- **A no-op step is dropped.** A step that is valid but would change nothing is reported in `skipped`, takes no part in the overlap check, and is not an error. It must still resolve and pass its checks.
- **`PlanError { entry, origin, cause }`** carries the failing step. `PlanCause` is `Address`, `Edit` or `Overlap`.
- **Programs.** NED programs are evaluated with no store lock held. The mutating builtins (`ned/set-text` and the rest) record a step into a `PlanRecorder` and return nil; they write nothing. Dependent steps are not supported, in two ways:
  - A step that uses an earlier mutation's result gets nil, and the builtin fails with an evaluation error. Nothing is applied.
  - A read in the same program sees the store as it was before the program. For example, `(do (ned/delete X) (ned/count X))` still counts X. This is select-then-apply semantics and is documented, not an error.
- **`ned/store` is removed.** A program can no longer reach the store directly.
- **`ned/parse-body`** is pure: it parses markdown into node trees with the shared parser, refuses comments and non-payload kinds, and writes nothing. It is not bound in the query-only root that evaluates addresses (suggestion selections and BlockEdit selections); an address never needs block trees.

### 3. Suggestions and block edits are one-step plans

- Suggestion accept and BlockEdit Apply run as a plan of one step (`apply_ned_mutation` in the conductor). Under one write lock the conductor does, in order: `resolve`, the file check, the "already in place" check, `check_drift`, then `execute`. The file check comes first because a block in another document is never "in place" and is never drift. `execute` re-resolves the same selection under the same lock, so it cannot disagree with the conductor. The drift check stays in the conductor until suggestions carry plans (channel gap 10).
- **A no-op counts as applied.** Accepting a suggestion whose content already reads as proposed marks it `Accepted`, changes nothing, and rebuilds nothing (commit 8a179cc). Creating such a suggestion answers "already in place" and stores nothing (commit 8d26d1f, `Response::AlreadyInPlace`). A block edit whose proposal is already in place answers `NoChange`, for both intents.
- **Slot and body edits go through BlockEdit Apply.** The `EditSlot` fast path for single-text slots is a BlockEdit Apply (ADR-059 §10). `Command::EditBodyElement` and `/_presemble/edit-body` are removed; the scroll anchor after a body edit is computed by BlockEdit Apply (ADR-046). `EditSlot` survives only for the escape hatch and the link picker (ADR-041, ADR-047) until stage 5b.
- **Limitation.** "Already in place" is only detected for a single block. A structural (multi-block) suggestion whose result is already in place goes `Stale` with `Drifted`.

### 4. Validity is reported, never enforced

Validity is reported when the store is populated: `populate_node_store` validates each document against its grammar, and the diagnostics make the document unpublishable (ADR-061 gates publishing). Re-validating a document after each edit is not implemented yet (open question 4): the edit path (`mark_dirty_and_rebuild`, then `rebuild_pages_for_modified_nodes`) only re-renders the affected pages. Edits, suggestion accepts and saves are never refused on grammar grounds.

Rejecting schema-invalid edits would make intermediate states impossible to reach, such as half-filled required slots or reordered sections. Editors would be forced to work in schema-valid jumps. Publishing is the real safety boundary.

### 5. Saving is total

`serialize_from_store` succeeds for any document whose blocks pass the postcondition of §1. A missing `preamble` or `body` serializes as empty. Getting the text right is the serializer's job (canonical escaping, ADR-056 §3 as amended), not a reason to refuse an edit.

### 6. Errors

`StaleCause` is the shared vocabulary for why an edit cannot apply. There is one mapping per surface:

- suggestions: `StaleCause`, via `From<EditError>`, `From<AddressError>` and `From<PlanError>`;
- block edits: `BlockEditCause`, via `apply_refusal_cause`, the one `StaleCause`-to-`BlockEditCause` mapping;
- the REPL: the `Display` of the error (`step 2 (ned/set-text): ...`);
- `/apply`: a structured `PlanFailure { step, origin, cause, message }` as additive response fields (commit 148e749). `StaleCause::Overlap { with_step }` is the cause of a step that changes what an earlier step changes; only multi-step programs can produce it, so a persisted suggestion never carries it.

### 7. Recovery

- **A broken commit invariant is a bug and panics.** `commit` cannot fail on input, so a failure there is never something to recover from automatically. Nothing rolls back automatically.
- **Manual recovery is `presemble reload <site-dir> <file>`** (commit 2ca4218; `Command::ReloadDocument`, `Response::Reloaded { discarded, editor_buffer }`). It rebuilds one document from its file on disk. `discarded` says the conductor held unsaved edits for it, now gone. `editor_buffer` says an editor also holds unsaved text for it; that is the editor's and stays there. It is the way to take the disk version of a conflicted document (below).
- **Unsaved edits are never discarded automatically** (ADR-065):
  - rebuilding the store from disk keeps every dirty document except those whose own file changed (commit 540d323);
  - a dirty document whose file changes on disk is a conflict, and saving over it needs consent that names the paths: save-all warns first, then offers Overwrite or Cancel (commit ab83d7e). ADR-065 has the rules; this ADR does not restate them;
  - content creation records its own writes as self-writes, so the watcher does not treat them as external changes (commit 17c3d92, ADR-043).

## Alternatives considered

- **Clone, journal and swap (the first version of this ADR):** rejected. A copy-on-write clone makes rollback cheap, but the journal, the scope rules and the commit gate duplicated what typed `prepare` and an infallible `commit` guarantee, and a program's own steps still had no select-then-apply semantics.
- **Reload from disk as automatic rollback:** rejected. It would discard unsaved work, which ADR-065 forbids. Reload is a manual recovery only.
- **"Apply to every match":** rejected. A selection that matches several nodes is refused, never applied to all of them (every mutation addresses exactly one node of the right kind).
- **Grammar gate on edits:** rejected; it contradicts the principle that invalid content is editable.
- **Serializer writes a best-effort form of unknown blocks:** rejected. There is no faithful form; it would silently mangle data on save.

## Consequences

- Invalid documents round-trip through disk. Markdown does not record slot names, so a document with an empty required slot may re-ingest with its blocks assigned to different slots. Block order and content are preserved, and diagnostics reflect the result.
- A bad payload or a refused step never leaves the store half-edited.
- Every channel that edits goes through `execute`, so the error model and the overlap rule are the same everywhere.

## Channel gaps still open

Each has the work that closes it:

- **Gap 6:** M1.
- **Gap 7:** stage 5b (the link picker and the `EditSlot` escape hatch).
- **Gap 8** (LSP buffer edits bypass NED, so buffers held only by the editor, the LSP mirror `doc_sources`, are not tracked as conflicts) and **gap 9:** the auto-save ADR. The editor owns those buffers and resolves its own conflicts until then (ADR-065).
- **Gap 10:** plans as the suggestion payload. Until then the drift check stays in the conductor.
- **The nREPL publish path:** closed by S2 (`refactor(editor_server)!: the nREPL runs its plans on the conductor's command loop`): the nREPL's plans run on the daemon's single writer thread through `Conductor::execute_plan`, and the events of every job are published.
- **An external write not yet reported by the watcher** (150 ms debounce) could be overwritten by a save. Closed by follow-up S1 (`fix(conductor): a save checks the file on disk before overwriting it`): a save compares the file's stamp with the one recorded when the conductor last read or wrote it, and a difference is a conflict (ADR-065, save-time mtime check).

## Closed in stage 2e

- **Gap 3, `Command::EditBodyElement`:** a positional body edit (`body_idx`) kept for older clients. It composed its own `ned/replace` program with `ned/parse-body`. It was removed in commit d6dce54; body edits go through BlockEdit Apply.
- **A no-op suggestion** is applied (accept) or answered "already in place" (create), instead of going stale (§3).
- **The `EditSlot` fast path** for single-text slots is a BlockEdit Apply; only the escape hatch and the link picker remain on `EditSlot` (§3).

## Open questions

1. **Undo scope.** Record a bounded history now with no undo command, or ship per-document undo.
2. **Page-body leak.** `serve`'s public file handler serves unsaved edits, working-tree-only pages and build errors to anonymous readers. Options: document it as a known gap now and gate HTML in Hosted mode later, or gate it now.
3. **Slot reshuffle on reload.** Should markdown record slot boundaries (for example an HTML comment marker), or is reassignment plus diagnostics acceptable?
4. **Validity after an edit.** Validity is reported only when the store is populated (§4). Should each edit re-validate the affected documents and refresh their diagnostics, so the unpublishable state follows the edit without a repopulate?

Resolved since the first version:

- **Well-formedness line:** an edit may not leave a block the serializer cannot write; enforced by the postcondition of §1.
- **Lossy canonicalization:** accepted. Nodes are the primary model and markdown is a serialization (ADR-059 §0 and §7).
- **Inline markup in stored text:** resolved by ADR-059 (inline content is nodes) and ADR-063 (headings and blockquotes store markdown source).

## Related

ADR-040, ADR-041, ADR-043, ADR-046, ADR-047, ADR-054, ADR-056, ADR-059, ADR-061, ADR-063, ADR-065; memory `feedback_invalid_is_saveable`.
