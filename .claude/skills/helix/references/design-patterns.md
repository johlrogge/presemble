# TUI Keymap Design Patterns (Helix-Informed)

## The Layer Model

Organize keys in three concentric layers by frequency of use:

```
Layer 1 — Normal mode, bare keys
  → Highest frequency: navigation, selection, primary actions
  → User reaches these without thinking

Layer 2 — Minor modes (single-key prefix, non-sticky)
  → Medium frequency: domain-specific operations
  → User thinks "I need to do an X-thing" → press X prefix → single key

Layer 3 — Leader / sticky mode (Space or equivalent)
  → Low frequency: meta-operations, configuration, help
  → User stops and thinks → presses leader → browses available commands
```

## What Belongs in Each Layer

### Layer 1 (Normal Mode, Bare Keys)
- Navigation: up/down/left/right through the primary list
- Primary action: play, select, open (Enter or a prominent key)
- Toggle: play/pause, mute (Space — but check if your app uses Space as leader)
- Quick undo/redo: `u`/`U` if destructive actions are common
- Delete/remove: `d`
- Search/filter: `/`
- Quit: `q` (modal TUIs conventionally use `q`)

### Layer 2 (Minor Mode Prefixes)
Group by domain noun:

| Prefix | Domain | Example bindings |
|--------|--------|-----------------|
| `q` | Queue ops | `qa` add, `qd` delete, `qc` clear, `qm` move |
| `p` | Playlist ops | `pn` new, `pd` delete, `pr` rename, `pa` add-current |
| `l` | Library/source | `la` add source, `lr` rescan, `ls` sort |
| `s` | Settings/sort | `ss` by name, `sa` by artist, `sl` shuffle |

**Convention**: prefix key = first letter of the domain noun.

### Layer 3 (Sticky Leader)
- Help: `?`
- Theme/appearance
- Configuration
- Save state / export
- About / version info

## The Selection-First Principle Applied to TUIs

A list is just a vertical buffer. Apply the same grammar:

1. **Navigate** to items (hjkl or arrows)
2. **Select** one or many (Enter to select, `C` to add cursor, `s` to filter)
3. **Act** on selection (`d` delete, `y` yank/copy, `p` paste/add-to-queue, Enter play)

This means:
- `d` on a queue item = remove it
- `y` on a track = add to clipboard / remember for later
- `p` = add remembered track to current list
- Multi-select → batch operation (delete many tracks at once)

## Conventions Helix Users Expect

These patterns are so deeply trained that violating them creates friction:

| Key | User Expectation | Notes |
|-----|-----------------|-------|
| `h`/`j`/`k`/`l` | Navigate left/down/up/right | Sacred — never remap |
| `gg` | Go to first item | Or `g` prefix for goto things |
| `G` | Go to last item | Very common convention |
| `d` | Delete/remove | "Delete the thing" |
| `y` | Yank/copy | "Remember the thing" |
| `p` | Paste/add | "Put the thing" |
| `u` | Undo | |
| `/` | Search/filter | |
| `n`/`N` | Next/previous search result | |
| `q` | Quit / close panel | |
| `?` | Help | |
| `Escape` | Return to previous state / normal mode | |
| `Enter` | Confirm / open / play | |
| `i` | Enter edit/insert | If your app has editable fields |

## What NOT to Put in Normal Mode

Things that should **not** be bare keys in normal mode:

- Destructive operations without confirmation (use a prefix + key)
- Multi-step operations (put in a mode where user sees a menu)
- Configuration (belongs in leader/space mode)
- Anything that benefits from discoverability (put in a mode with help popup)

## Mode Design: Sticky vs Non-Sticky

**Use non-sticky for**: "I want to do one X-thing and get back"
- Queue: add one track, return to browsing
- Jump to artist, return to normal navigation
- Rename, return to list

**Use sticky for**: "I'm going to do several X-things in a row"
- Managing a playlist (multiple rename/reorder/delete operations)
- Configuring settings (multiple toggles)
- The leader/help menu (browsing what's available)

## Discoverability

Helix shows a help popup when you enter space mode — steal this pattern:

When user enters a minor mode:
1. Show a floating popup listing all bindings for that mode
2. Header: mode name ("Queue Mode", "Playlist Mode")
3. Format: `key — description` pairs
4. Disappears when user presses a key or Escape

This is why non-obvious keys are okay in minor modes — they're self-documenting.

## Status Line / Mode Indicator

Follow Helix's mode indicator pattern:

```
[NORMAL] ──────────────────── Artist: Radiohead | Track: 3/50 | 3:12 / 5:01 | Vol: 80%
[QUEUE]  ──────────────────── q: add  d: delete  c: clear  m: move  ?: help
```

- Always show current mode prominently (left side)
- In minor modes, show available keys in the status bar as hints
- This reduces reliance on memory for minor mode bindings

## Conflict Checklist

Before finalizing a keymap, check each key against:

1. **Helix normal mode defaults** — `h`, `j`, `k`, `l`, `w`, `b`, `e`, `d`, `c`, `y`, `p`,
   `u`, `U`, `v`, `f`, `t`, `F`, `T`, `r`, `x`, `X`, `o`, `O`, `J`, `K`, `/`, `?`,
   `n`, `N`, `*`, `s`, `S`, `C`, `;`, `,`, `(`, `)`, `>`, `<`, `=`, `~`, `` ` ``,
   `q`, `Q`, `%`, `&`, `|`, `!`, `@` — almost everything is taken

2. **Helix prefix keys** — `g`, `m`, `z`, `Z`, `[`, `]`, `'`, `"`, `Ctrl-w`, `Space`

3. **Terminal reserved** — `Ctrl-C`, `Ctrl-Z`, `Ctrl-S` (flow control in some terminals)

4. **Cross-platform concerns** — some terminals eat `Alt-*` combos; `Ctrl-*` with symbols
   may not work everywhere

## Example: Music Player (MDMA) Keymap Design

```
Normal Mode:
  h/j/k/l or arrows    — Navigate list
  Enter                — Play selected track
  Space                — Play/pause toggle
  d                    — Remove from current list
  u                    — Undo last change
  /                    — Search / filter
  n / N               — Next/prev search result
  q                    — Quit
  ?                    — Help (or leader+?)
  1-4 (or Tab)         — Switch panel (library/queue/playlist/settings)

Queue prefix (q):
  qa  — Add selected track to queue
  qd  — Remove from queue
  qc  — Clear queue
  qn  — Move to next position
  qp  — Move to previous position
  q?  — Queue mode help

Playlist prefix (p):
  pn  — New playlist
  pd  — Delete playlist
  pr  — Rename playlist
  pa  — Add selected to playlist
  po  — Open/select playlist
  p?  — Playlist mode help

Sort prefix (s):
  ss  — Sort by song name
  sa  — Sort by artist
  sl  — Toggle shuffle
  sr  — Toggle repeat
  s?  — Sort mode help

Leader/sticky (Space or \):
  ?   — Full help
  t   — Theme picker
  v   — Volume adjustment
  s   — Settings
  e   — Equalizer
```

## The "What Mode Am I In?" Problem

Vim's biggest UX failure: users don't know what mode they're in.

Helix solves this with:
1. Prominent mode indicator in status line
2. Cursor shape changes per mode (block in normal, bar in insert)
3. Color scheme changes per mode

For TUIs: use at minimum #1 and #2. Mode confusion is the top cause of user errors in modal UIs.
