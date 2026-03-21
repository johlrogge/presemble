# Helix Mode Architecture

## The Three Root Modes

Helix has three primary modes — far fewer than Vim's many:

| Mode | Entry | Exit | Purpose |
|------|-------|------|---------|
| **Normal** | `Escape` from any mode | — | Navigation, selection, actions |
| **Insert** | `i`, `a`, `I`, `A`, `o`, `O` | `Escape` | Text entry |
| **Select/Extend** | `v` | `Escape` | Extend selections rather than replace |

**Design principle**: Normal mode is the home. You always return here. Keep it.

## Normal Mode Minor Modes (Prefix Keys)

Minor modes are nested keymaps within normal mode, activated by a prefix key.
They are **not** separate modes — they are one-level-deep subkeymaps.

### Sticky vs Non-Sticky

**Non-sticky** (single command, then returns to normal mode):
- Press prefix → press one key → back to normal
- Good for: targeted, infrequent operations

**Sticky** (remains active until `Escape`):
- Press prefix → press many keys without re-pressing prefix → `Escape` to exit
- Good for: command menus, exploratory workflows, multiple related operations

### Built-in Prefix Keys (DO NOT CONFLICT)

| Prefix | Mode | Sticky? | Domain |
|--------|------|---------|--------|
| `g` | Goto mode | No | Jump to locations (definitions, lines, files) |
| `m` | Match mode | No | Surround, text objects |
| `z` | View mode | No | Scroll/view (non-sticky single command) |
| `Z` | View mode | Yes | Scroll/view (sticky, for reviewing) |
| `Ctrl-w` | Window mode | No | Split management and navigation |
| `Space` | Leader/Space mode | Yes | LSP, pickers, meta-commands |
| `[` | Bracket prev | No | Navigate to previous (function, type, etc.) |
| `]` | Bracket next | No | Navigate to next |
| `'` | Register select | No | Choose register for yank/paste |
| `"` | Register select (alt) | No | Same as `'` |

**These prefix keys are taken in every Helix installation.** Any TUI that targets Helix users
should not shadow these in its normal mode without excellent justification.

## Goto Mode (`g`) — Non-Sticky

Second key hints at target destination.

| Key | Action |
|-----|--------|
| `gd` | Go to definition (LSP) |
| `gr` | Go to references (LSP) |
| `gi` | Go to implementation (LSP) |
| `gy` | Go to type definition (LSP) |
| `ge` | Next diagnostic |
| `gE` | Previous diagnostic |
| `gg` | Go to line number |
| `g.` | Go to last modification |
| `ga` | Go to alternate file |
| `gh` | Start of line |
| `gl` | End of line |
| `gs` | Go to first non-whitespace |
| `gt` | Go to top of screen |
| `gm` | Go to middle of screen |
| `gb` | Go to bottom of screen |

**Pattern**: `g` + semantic hint for destination.

## Match Mode (`m`) — Non-Sticky

Handles text objects and surround operations.

| Key | Action |
|-----|--------|
| `mi<obj>` | Select inside text object |
| `ma<obj>` | Select around text object |
| `ms<char>` | Surround selection with char pair |
| `mr<old><new>` | Replace surrounding pair |
| `md<char>` | Delete surrounding pair |
| `mm` | Go to matching bracket |

**Text object keys**: `w` word, `W` WORD, `p` paragraph, `(` `[` `{` `<` pairs,
`"` `'` `` ` `` quote pairs, `m` nearest pair, `f` function, `t` type, `a` argument,
`c` comment, `T` test, `g` change.

## View Mode (`z`/`Z`) — Non-Sticky / Sticky

Scroll and view manipulation without moving the cursor.

| Key | Action |
|-----|--------|
| `zt` / `zz` | Scroll cursor to top / center |
| `zb` | Scroll cursor to bottom |
| `zk` / `zj` | Scroll up/down one line |
| `Ctrl-u`/`Ctrl-d` | Half page up/down |
| `Ctrl-b`/`Ctrl-f` | Full page up/down |

## Window Mode (`Ctrl-w`) — Non-Sticky

| Key | Action |
|-----|--------|
| `Ctrl-w s` | Horizontal split |
| `Ctrl-w v` | Vertical split |
| `Ctrl-w w` | Cycle through windows |
| `Ctrl-w c` | Close window |
| `Ctrl-w o` | Close all other windows |
| `Ctrl-w h/j/k/l` | Focus left/down/up/right |
| `Ctrl-w =` | Equalize splits |
| `Ctrl-w +/-` | Resize height |
| `Ctrl-w >/<` | Resize width |

## Space Mode (Leader) — Sticky

Space mode is Helix's "command palette prefix." It stays active after each command.

Default Space bindings (community-standard defaults):

| Key | Action |
|-----|--------|
| `Space f` | Find file (fuzzy picker) |
| `Space b` | Open buffer |
| `Space /` | Search in project (grep) |
| `Space g` | Goto grep result |
| `Space d` | Diagnostics picker |
| `Space a` | Code action (LSP) |
| `Space w` | Workspace symbols |
| `Space r` | Rename symbol (LSP) |
| `Space ?` | Help / show keybinds |
| `Space y` | Yank to clipboard |
| `Space p` | Paste from clipboard |
| `Space k` | Show hover doc (LSP) |
| `Space s` | Symbol picker |
| `Space S` | Workspace symbol picker |
| `Space e` | Open file explorer |

**Design insight**: Space mode is for "I need to think about what to do" commands — discovery-oriented,
infrequent, high-value operations. The sticky behavior lets you chain: find file, then switch buffer,
without re-pressing Space.

## Select/Extend Mode (`v`) — Root Mode

Mirrors normal mode but all movement commands *extend* the selection rather than replacing it.

- `v` then `gl` = extend selection to end of line
- `v` then `3j` = extend selection down 3 lines
- `v` then `f,` = extend selection to next comma

Combine with `s` (split selection by regex) for powerful multi-cursor workflows.

## Insert Mode Entry Commands

These transition from normal → insert with different cursor semantics:

| Key | Behavior |
|-----|----------|
| `i` | Insert *before* selection |
| `a` | Append *after* selection (extends selection as you type) |
| `I` | Insert at start of line |
| `A` | Append at end of line |
| `o` | Open line *below*, enter insert |
| `O` | Open line *above*, enter insert |
| `s` → insert | After splitting selection, insert at each cursor |
| `c` | Change: delete selection, enter insert |

**Key semantic**: `a` extends the selection as you type — intentional for multi-cursor consistency.

## Mode Naming Conventions

When creating custom modes for domain-specific TUIs:

- Use short, memorable verbs or nouns as prefix keys
- Show a help popup when entering the mode (name the mode in the popup header)
- Non-sticky for "peek" modes (quick single action), sticky for "command" modes (multiple actions)
- Avoid any key already used as a prefix in Helix normal mode (`g`, `m`, `z`, `Z`, `[`, `]`, `Ctrl-w`, `Space`, `'`, `"`)
