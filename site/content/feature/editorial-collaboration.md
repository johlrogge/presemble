# Editorial Collaboration

Claude and human editors suggest changes; you accept or reject them in your editor.

Presemble's editorial suggestion protocol lets external collaborators — Claude via the MCP server, or human editors via the conductor — push suggestions into your content as LSP diagnostics. Each suggestion appears as a warning in your editor with a code action to accept or reject it. No content changes until you decide.

----

### How suggestions work

A suggestion is a structured change to a content slot. It carries the target file, the slot name, the proposed value, and a rationale. The conductor receives the suggestion, stores it in memory, and forwards it to the LSP server as a diagnostic. The author sees it inline, reads the rationale in the hover tooltip, and uses the code action to accept or discard.

Accepted suggestions write the new value to the content file. Rejected suggestions are discarded. The conductor tracks dirty buffers — pending edits that have been accepted but not yet saved to disk.

### Claude integration via MCP

`presemble mcp site/` starts an MCP server that exposes the site to Claude Code:

- `get_content` — read a content file
- `get_schema` — read a schema
- `suggest` — push a suggestion for a named slot
- `list_content` — enumerate content files by type (wired to conductor `ListContent`)

Each tool accepts an optional `site` parameter. If you configure the MCP server globally rather than per-project, Claude passes the site directory on each call — no restart required to switch between sites.

Claude can read your schemas to understand your content model, read your content files to understand what exists, and push targeted suggestions to specific slots with a rationale. The author sees each suggestion as an LSP diagnostic and decides whether to accept it.

This is the same suggestion protocol a human editor uses. There is no special Claude path — Claude is just another collaborator using the suggestion API.

### Slot-scoped suggestions (SlotEdit)

In addition to full-slot replacement suggestions, the conductor accepts `SuggestSlotEdit` — a search/replace suggestion that targets part of a slot's content. This lets a collaborator correct one sentence in a long summary or fix a single phrase in a body section without proposing a full rewrite. Both kinds appear as LSP diagnostics with accept/reject code actions and as inline diffs in the browser.

### Browser suggestion preview

In `presemble serve` mode, pending suggestions appear as inline diffs in the browser. A toolbar at the top of the page shows how many suggestions are pending. Each suggestion node displays the current value alongside the proposed value. The preview toggle switches between the current state and what the page would look like if all suggestions were accepted.

The mascot overlay indicates the current editorial state: suggestions present, all clear, or edit mode active.

### nREPL for programmatic access

`presemble nrepl site/` starts an nREPL server that Calva and CIDER can connect to. From a connected REPL you can evaluate expressions against the live content graph, call suggestion operations programmatically, and inspect the site's data model interactively.

See [the REPL](/feature/the-presemble-repl) for full details.
