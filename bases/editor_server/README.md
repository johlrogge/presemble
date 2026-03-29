# editor_server

Stub base for the multiplayer editing service (in progress, M4+).

Will host the conductor process that owns the dep_graph, schema cache, file watcher, and in-memory working copies of content files. `presemble lsp` and `presemble serve` will become thin clients of this conductor, connected via nng IPC.

See [ROADMAP.md](../../ROADMAP.md) — M4 for the full design.

---

[Back to root README](../../README.md)
