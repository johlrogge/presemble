# editor_server

Stub base for the long-running multiplayer editing service (M4+).

Will back the `content_management` project — a hosted conductor that multiple authors can connect to simultaneously. The in-process conductor already available in `presemble serve` handles single-author use; `editor_server` extends this for multi-author sessions.

See [ROADMAP.md](../../ROADMAP.md) for the full design.

---

[Back to root README](../../README.md)
