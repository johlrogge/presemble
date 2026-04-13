# ADR-039: Self-contained REPL with documentation metadata

## Status

Accepted

## Context

Presemble has a working evaluator (ADR-036) with ~45 Rust primitives and ~50 prelude functions in `core.clj`. However:

1. **No metadata system.** The `DOCS` constant in `evaluator/src/lib.rs` is a hardcoded `&[(&str, &str, &str)]` table covering only Rust primitives. The 50 functions defined in `core.clj` have zero documentation. The `defn` macro recognizes docstrings syntactically but silently discards them.

2. **No self-contained REPL.** The nREPL server exists but requires an external client (Calva, CIDER, rep). There is no way to explore Presemble without third-party tooling.

3. **No completion or doc-lookup protocol.** The nREPL `describe` op advertises only `eval`, `clone`, `close`, `describe`. No `complete` or `info` ops.

These gaps block three user stories: terminal exploration without external tools, browser-based template editing with inline docs, and discoverability of the growing function library.

### Related ADRs

- **ADR-036** (Evaluator bootstrap): Defines the evaluator, environment, `defn` macro, and `Callable` trait.
- **ADR-031** (Conductor as sole authority): The conductor owns the environment. The REPL connects to it.
- **ADR-035** (Workspace conductor): Conductor is per-workspace, not per-site.

## Decision

### 1. Documentation metadata via a registry in the root environment

**Approach: parallel registry, not inline on Value.**

A `DocEntry` struct lives in a new `doc_registry` module inside the `evaluator` component:

```rust
pub struct DocEntry {
    pub name: String,
    pub doc: String,
    pub arglists: Vec<String>,   // e.g. ["[coll]", "[n coll]"]
    pub source: DocSource,
}

pub enum DocSource {
    Primitive,
    Prelude,
    User,
}
```

The registry is a `HashMap<String, DocEntry>` stored alongside the root environment bindings, populated at three moments:

1. **Primitive registration** (`primitives::register_builtins`): Each primitive call gains a doc parameter. The current `DOCS` constant is retired.
2. **Prelude loading** (`load_prelude`): The `defn` macro expansion preserves the docstring via a `def-doc!` special form.
3. **User `defn` at the REPL**: Same path as prelude.

**Why not metadata on Value?** Documentation is about *names*, not *values*. A function passed as an argument to `map` should not carry its docstring through every intermediate binding. A registry keyed by name is simpler and matches Clojure's var-metadata model.

### 2. Fix `defn` macro to preserve docstrings

When a docstring is present:
```clojure
(defn name "docstring" [args] body)
```
expands to:
```clojure
(do
  (def name (fn name [args] body))
  (def-doc! name "docstring" "[args]"))
```

`def-doc!` is a special form handled by the evaluator that writes to the doc registry. For multi-arity `defn`, arglists are extracted from each clause.

### 3. Completion protocol via nREPL ops

Add `completions` and `info` ops to the nREPL server:

- `completions`: takes a prefix, returns matching symbols with doc summaries
- `info`: takes a symbol name, returns full doc entry

The `NreplHandler` trait gains:
```rust
fn completions(&self, session: &str, prefix: &str) -> Vec<CompletionEntry>;
fn doc_lookup(&self, session: &str, symbol: &str) -> Option<DocEntry>;
```

### 4. TUI REPL with ratatui

A ratatui-based TUI rather than readline. Key reasons:
- Completions show in a popup panel, not inline
- Persistent documentation panel shows docs for symbol under cursor
- Select + evaluate regions (like notebook cells)
- Separate panels for input, output, documentation

```
+------------------------------------------------------+
| Presemble REPL                                        |
+------------------------------------------------------+
|  Output panel (scrollable)                             |
|  > (map inc [1 2 3])                                   |
|  (2 3 4)                                               |
+------------------------------------------------------+
|  Doc panel (symbol under cursor)                       |
|  map                                                   |
|    (map f coll)                                        |
|    Apply f to each element of coll.                    |
+------------------------------------------------------+
|  > (filter even? (range 10))                    |
|                              +------------------+      |
|                              | even?            |      |
|                              | every?           |      |
|                              +------------------+      |
+------------------------------------------------------+
```

**Key interactions:**
- **Enter**: evaluates when delimiters are balanced and input is non-empty; otherwise inserts newline
- **Ctrl+J**: force-eval regardless of balance state
- **Alt+Enter**: force-eval (terminals that support it)
- **Ctrl+O**: force-insert newline regardless of balance state
- **Tab**: trigger completion
- **Ctrl+D**: quit
- **Ctrl+L**: clear output
- EDN syntax highlighting in input
- Up/Down: history navigation

### 5. Component structure

| Component | Type | Purpose |
|---|---|---|
| `repl_tui` | new component | TUI rendering, input, completion popup, doc panel |
| `evaluator` | existing | Gains `DocRegistry`, `def-doc!` special form |
| `nrepl` | existing | Gains `completions` and `info` ops |
| `macros` | existing | `defn` expansion preserves docstrings |

No new base — the `repl` subcommand goes into `publisher_cli`.

### 6. Two REPL backends

```rust
pub trait ReplBackend: Send + Sync {
    fn eval(&self, code: &str) -> Result<EvalResult, String>;
    fn completions(&self, prefix: &str) -> Vec<CompletionEntry>;
    fn doc_lookup(&self, symbol: &str) -> Option<DocEntry>;
    fn all_symbols(&self) -> Vec<String>;
}
```

- **`NreplBackend`** — connects to a running conductor via nREPL TCP. Site-aware.
- **`DirectBackend`** — in-process evaluator, no conductor. For exploring the language.

`presemble repl` auto-detects: if `.nrepl-port` exists, use nREPL; otherwise, bare mode.

## Alternatives considered

- **Metadata on `Value::Fn`** — Rejected. Docs are about names, not values. A registry is simpler.
- **Full `^{:doc ...}` Clojure metadata** — Deferred. We get 95% of the value from docstrings on `defn`.
- **Simple readline REPL** — Rejected. Can't show persistent doc panel, completion popup, or output history.
- **Separate REPL binary** — Unnecessary. `presemble repl` subcommand keeps the UX unified.
- **WebSocket for browser** — Deferred. nREPL protocol handles eval/completions/doc. A WS-to-nREPL bridge is a thin adapter for later.

## Consequences

### Positive

- **Self-contained.** `presemble repl` works out of the box — no external tools.
- **Discoverable.** Every function has docs via `(doc ...)`, tab completion, and the doc panel.
- **Shared protocol.** `ReplBackend` trait and nREPL ops serve both TUI and future browser editor.
- **Incremental.** Phase A (metadata) improves existing nREPL clients immediately.

### Negative

- **ratatui dependency.** Only pulled into `repl_tui` component, not the publisher or editor.
- **Docstring maintenance.** ~50 prelude functions need docstrings. One-time cost.
- **Two backends.** `NreplBackend` and `DirectBackend` are two code paths, but the trait keeps them interchangeable.
