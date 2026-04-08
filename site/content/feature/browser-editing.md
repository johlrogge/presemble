# Browser Editing

Edit content directly in the browser — no editor required.

`presemble serve` turns the browser into a live editing environment. Click any body element to open an inline textarea. Fill in missing slots from a suggestion node. Accept or reject editorial suggestions with a toolbar button. Create new content pages with the "+" button — without touching the filesystem directly.

----

### Inline body editing

Click any rendered body element in serve mode to enter edit mode for that element. An inline textarea opens with the current markdown source. Save closes the textarea and triggers a live rebuild; the updated content appears in the browser within a second.

The mascot overlay in the corner of the page shows the current mode:

| State | Indicator |
|---|---|
| All clear | Thumbs up |
| Suggestions present | Badge with count |
| Edit mode active | Pencil |

### Suggest mode

The mascot popover offers three modes: View, Edit, and Suggest. In Suggest mode, missing slots render as inline suggestion nodes with the schema's hint text. Clicking a suggestion node opens an editing form pre-filled with the hint. Fill in the value and save — the slot is written to the content file and the suggestion node disappears.

Suggest mode makes it possible to fill in a content file entirely from the browser, guided slot-by-slot by the schema's own hint text.

### Pending suggestion diffs

When a collaborator (or Claude via the MCP server) pushes suggestions, they appear as inline diffs in the browser alongside the current content. A toolbar shows the count of pending suggestions and offers "Accept all" and "Reject all" shortcuts. Individual suggestions can be accepted or rejected from the diff view.

The preview toggle switches between the current published state and a preview of what the page looks like with all suggestions applied.

### Create new content from the browser

The "+" button in the serve UI opens a form to create a new content file. Select a content type, enter a slug, and submit. The conductor scaffolds the file with the correct schema structure and opens it with all required slots present as suggestion nodes. The browser navigates to the new page immediately.

### Header folding in edit mode

In Edit mode, headings in the content display a fold toggle. Click the toggle to collapse or expand the section beneath that heading. Collapsed sections stay out of the way while you focus on another part of the page. Two toolbar buttons let you collapse all sections or expand them all at once.

Clicking anywhere on a collapsed heading section unfolds it. Fold state is not persisted across page reloads — the page always opens fully expanded.

### Dirty buffer tracking

Edits made in the browser or via accepted suggestions are held in the conductor's dirty buffer until explicitly saved. The mascot badge indicates unsaved changes. Save sends the dirty buffer contents to disk and clears the buffer. This separation lets you review several changes before committing any of them to the filesystem.

When a browser edit triggers a rebuild, the conductor resolves link expressions and cross-content references in the affected page. Feature cards, author links, and any content that depends on linked documents render correctly after a browser edit — no server restart needed.
