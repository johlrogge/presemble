# repl_tui

Full-screen terminal REPL for the Presemble expression language, built with [ratatui](https://github.com/ratatui-org/ratatui).

## What it does

Provides a TUI REPL with three panels — output history, a doc panel, and an input editor — and two operating modes:

- **Standalone** (`DirectBackend`): no conductor required; evaluates language primitives and prelude functions in-process. Site-specific operations (e.g. `query`) return informative errors.
- **Connected** (`NreplBackend`): connects to a running conductor's nREPL server over TCP; full site graph access, the same environment Calva or CIDER would use.

The `presemble repl` CLI command auto-discovers `.nrepl-port` by walking the current directory's parents. If a port file is found it uses `NreplBackend`; otherwise it falls back to `DirectBackend`.

## Key bindings

| Key | Action |
|---|---|
| Enter | Eval when delimiters are balanced; insert newline otherwise |
| Ctrl+J | Force-eval regardless of balance |
| Ctrl+O | Force-insert newline |
| Tab | Trigger completion popup |
| Up / Down | Navigate completion popup (when open) or command history |
| Esc | Dismiss completion popup |
| Ctrl+L | Clear output panel |
| Ctrl+D | Quit |

## Features

- EDN syntax highlighting (keywords, strings, numbers, brackets, comments)
- Completion popup with inline doc hints; accepts selected candidate on Enter
- Doc panel: shows arglists and doc string for the symbol under the cursor
- Command history with Up/Down navigation
- Delimiter balancing: Enter only evaluates when all `(`, `[`, `{` are closed and no string literal is left open; respects `"…\"…"` escaping and `;` line comments

## Building

This component is part of the `publisher` project and is built along with the workspace:

```
cargo build -p publisher
```

To run directly:

```
presemble repl                # standalone, or auto-connects if .nrepl-port is found
presemble repl --port 1667    # connect to a conductor on a specific port
```

## Structure

| File | Description |
|---|---|
| `src/app.rs` | TUI loop, key event handling, EDN highlighting, delimiter balancing |
| `src/backend.rs` | `ReplBackend` trait; `DirectBackend` (in-process); `NreplBackend` (nREPL over TCP) |
| `src/lib.rs` | Public re-exports |

Back to [root README](../../README.md).
