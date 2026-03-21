# Helix Complete Keybinding Reference

## Normal Mode — Movement

### Character and Line
| Key | Action |
|-----|--------|
| `h` / `Left` | Move left |
| `j` / `Down` | Move down |
| `k` / `Up` | Move up |
| `l` / `Right` | Move right |
| `gh` / `Home` | Start of line |
| `gl` / `End` | End of line |
| `gs` | First non-whitespace character |
| `^` | First non-whitespace (alias) |

### Word Navigation
| Key | Action |
|-----|--------|
| `w` | Next word start |
| `b` | Previous word start |
| `e` | Next word end |
| `W` | Next WORD start (whitespace-delimited) |
| `B` | Previous WORD start |
| `E` | Next WORD end |

### Character Finding (search in line and beyond)
| Key | Action |
|-----|--------|
| `f<char>` | Select to next occurrence (inclusive) |
| `t<char>` | Select to next occurrence (exclusive, stop before) |
| `F<char>` | Select to previous occurrence (inclusive) |
| `T<char>` | Select to previous occurrence (exclusive) |

### Screen / File Navigation
| Key | Action |
|-----|--------|
| `Ctrl-u` | Half page up |
| `Ctrl-d` | Half page down |
| `Ctrl-b` / `PageUp` | Full page up |
| `Ctrl-f` / `PageDown` | Full page down |
| `gg` | Go to first line |
| `ge` | Go to last line |
| `Ctrl-o` | Jump backward (jumplist) |
| `Ctrl-i` | Jump forward (jumplist) |

### Search
| Key | Action |
|-----|--------|
| `/` | Search forward (regex, real-time highlight) |
| `?` | Search backward |
| `n` | Select next search match |
| `N` | Select previous search match |
| `*` | Search word under cursor (with word boundaries) |
| `Alt-*` | Search word under cursor (without boundaries) |

---

## Normal Mode — Selection

### Expand / Shrink
| Key | Action |
|-----|--------|
| `v` | Enter select/extend mode |
| `x` | Expand selection to full lines |
| `X` | Shrink to lines (removes first line) |
| `%` | Select entire file |
| `Alt-x` | Shrink selection to inner content |

### Multiple Cursors
| Key | Action |
|-----|--------|
| `C` | Add cursor on line below (duplicate selection down) |
| `Alt-C` | Add cursor on line above |
| `s` | Split selection by regex (one cursor per match) |
| `S` | Select all matches in selection |
| `Alt-s` | Split selection on newlines |
| `Alt-k` | Keep selections matching regex (filter) |
| `Alt-K` | Remove selections matching regex |
| `&` | Align selections to same column |
| `;` | Collapse to single cursor (keep primary) |
| `Alt-;` | Flip anchor and cursor |
| `,` | Remove all selections except primary |
| `Alt-,` | Remove primary, keep others |
| `(` | Rotate primary selection to previous |
| `)` | Rotate primary selection to next |

### Jumps
| Key | Action |
|-----|--------|
| `Alt-i<obj>` | Select inside text object (shrink) |
| `Alt-a<obj>` | Select around text object (expand) |

---

## Normal Mode — Changes

### Delete
| Key | Action |
|-----|--------|
| `d` | Delete selection |
| `Alt-d` | Delete word forward (without yanking) |
| `Ctrl-h` / `Backspace` | Delete char before cursor (in insert mode) |

### Change
| Key | Action |
|-----|--------|
| `c` | Change (delete selection, enter insert mode) |
| `r<char>` | Replace each selected char with char |
| `R` | Replace selection with yanked text |
| `~` | Toggle case |
| `` Alt-` `` | Switch case |

### Yank and Paste
| Key | Action |
|-----|--------|
| `y` | Yank (copy) selection |
| `p` | Paste after cursor |
| `P` | Paste before cursor |
| `Alt-d` | Delete without yanking |
| `"<reg>` | Select register (before y/p) |

### Line Operations
| Key | Action |
|-----|--------|
| `o` | Open line below, enter insert |
| `O` | Open line above, enter insert |
| `J` | Join lines (remove newline between) |
| `K` | Show hover doc (LSP) |
| `>` | Indent selection |
| `<` | Dedent selection |
| `=` | Format / auto-indent (LSP/Tree-sitter) |

### Undo / Redo
| Key | Action |
|-----|--------|
| `u` | Undo |
| `U` | Redo |
| `Alt-u` | Undo selection (restore previous) |
| `Alt-U` | Redo selection |

### Insert Mode Entry
| Key | Action |
|-----|--------|
| `i` | Insert before selection |
| `a` | Append after selection |
| `I` | Insert at start of line |
| `A` | Append at end of line |
| `o` | Open line below |
| `O` | Open line above |

---

## Normal Mode — Misc

| Key | Action |
|-----|--------|
| `Ctrl-/` | Toggle line comment |
| `Ctrl-a` | Increment number |
| `Ctrl-x` | Decrement number |
| `q` | Record macro (into register) |
| `Q` | Replay macro |
| `:` | Enter command mode |
| `\|` | Pipe selection through shell command |
| `Alt-\|` | Pipe each selection separately |
| `!` | Insert shell command output before |
| `Alt-!` | Insert shell command output after |

---

## Insert Mode

Insert mode has minimal keybindings — most keys type text.

| Key | Action |
|-----|--------|
| `Escape` | Return to normal mode |
| `Ctrl-c` | Return to normal mode |
| `Backspace` / `Ctrl-h` | Delete character before cursor |
| `Delete` / `Ctrl-d` | Delete character under cursor |
| `Ctrl-w` | Delete word before cursor |
| `Ctrl-u` | Delete to start of line |
| `Ctrl-k` | Delete to end of line |
| `Ctrl-r <reg>` | Insert contents of register |
| `Ctrl-n` | Autocomplete next |
| `Ctrl-p` | Autocomplete previous |
| `Ctrl-x` | Autocomplete (various sources) |
| `Tab` | Indent / next completion |
| `Shift-Tab` | Dedent / previous completion |
| `Enter` | Newline |

---

## Select / Extend Mode

Mirrors normal mode movement but extends selection instead of replacing it.
All movement commands in normal mode work here with extending semantics.

Entered via `v` in normal mode. Exit with `Escape`.

---

## Goto Mode (`g` prefix — Non-Sticky)

| Key | Action |
|-----|--------|
| `gd` | Go to definition (LSP) |
| `gr` | Go to references (LSP) |
| `gi` | Go to implementation (LSP) |
| `gy` | Go to type definition (LSP) |
| `gD` | Go to declaration (LSP) |
| `ge` | Next diagnostic |
| `gE` | Previous diagnostic |
| `gg` | Go to line (prompt) |
| `gk` | Go to previous line |
| `gj` | Go to next line |
| `g.` | Go to last modification |
| `ga` | Go to alternate file |
| `gh` | Start of line |
| `gl` | End of line |
| `gs` | First non-whitespace |
| `gt` | Top of screen |
| `gm` | Middle of screen |
| `gb` | Bottom of screen |
| `gw` | Goto word (labels appear on screen words) |

---

## Match Mode (`m` prefix — Non-Sticky)

### Text Object Selection
| Key | Action |
|-----|--------|
| `mi<obj>` | Select inside text object |
| `ma<obj>` | Select around text object |

### Surround Operations
| Key | Action |
|-----|--------|
| `ms<char>` | Surround selection with matching pair |
| `mr<old><new>` | Replace surrounding pair |
| `md<char>` | Delete surrounding pair |
| `mm` | Go to matching bracket |

### Text Object Reference
| Object | Key |
|--------|-----|
| Word | `w` |
| WORD | `W` |
| Paragraph | `p` |
| Parentheses | `(` or `)` |
| Brackets | `[` or `]` |
| Braces | `{` or `}` |
| Angles | `<` or `>` |
| Double quote | `"` |
| Single quote | `'` |
| Backtick | `` ` `` |
| Nearest pair (auto) | `m` |
| Function | `f` |
| Type/Class | `t` |
| Argument/Param | `a` |
| Comment | `c` |
| Test block | `T` |
| Change/diff | `g` |

---

## Space Mode (`Space` prefix — Sticky)

| Key | Action |
|-----|--------|
| `Space f` | File picker |
| `Space F` | File picker (current directory) |
| `Space b` | Buffer picker |
| `Space j` | Jumplist picker |
| `Space /` | Global search (grep) |
| `Space g` | Debug (DAP) |
| `Space d` | Diagnostics picker |
| `Space D` | Workspace diagnostics |
| `Space a` | Code actions (LSP) |
| `Space r` | Rename symbol (LSP) |
| `Space k` | Hover documentation (LSP) |
| `Space K` | Hover diagnostics |
| `Space s` | Document symbols |
| `Space S` | Workspace symbols |
| `Space e` | File explorer |
| `Space ?` | Help menu / keybind reference |
| `Space y` | Yank to system clipboard |
| `Space Y` | Yank line to clipboard |
| `Space p` | Paste from clipboard |
| `Space P` | Paste before from clipboard |
| `Space h` | Select references (LSP, highlight) |
| `Space w` | Window mode (same as Ctrl-w) |

---

## Window Mode (`Ctrl-w` prefix — Non-Sticky)

| Key | Action |
|-----|--------|
| `Ctrl-w s` | Horizontal split |
| `Ctrl-w v` | Vertical split |
| `Ctrl-w w` | Cycle windows |
| `Ctrl-w p` | Previous window |
| `Ctrl-w n` | New file in split |
| `Ctrl-w c` / `q` | Close window |
| `Ctrl-w o` | Close all other windows |
| `Ctrl-w h` / `Left` | Focus left |
| `Ctrl-w j` / `Down` | Focus down |
| `Ctrl-w k` / `Up` | Focus up |
| `Ctrl-w l` / `Right` | Focus right |
| `Ctrl-w =` | Equalize split sizes |
| `Ctrl-w +` / `-` | Increase/decrease height |
| `Ctrl-w >` / `<` | Increase/decrease width |
| `Ctrl-w H` | Swap with left |
| `Ctrl-w J` | Swap with below |
| `Ctrl-w K` | Swap with above |
| `Ctrl-w L` | Swap with right |

---

## View Mode (`z` / `Z` prefix — Non-Sticky / Sticky)

| Key | Action |
|-----|--------|
| `zt` | Scroll to top (cursor stays) |
| `zz` | Scroll to center |
| `zb` | Scroll to bottom |
| `zk` | Scroll view up one line |
| `zj` | Scroll view down one line |

---

## Bracket Navigation (`[` / `]` prefix — Non-Sticky)

Navigate between occurrences of structural elements:

| Key | Action |
|-----|--------|
| `[d` / `]d` | Previous/next diagnostic |
| `[f` / `]f` | Previous/next function |
| `[t` / `]t` | Previous/next type definition |
| `[a` / `]a` | Previous/next argument |
| `[c` / `]c` | Previous/next comment |
| `[T` / `]T` | Previous/next test |
| `[p` / `]p` | Previous/next paragraph |
| `[g` / `]g` | Previous/next change (git diff) |
| `[e` / `]e` | Previous/next diagnostic error |
| `[s` / `]s` | Previous/next spelling error |
| `[Space` / `]Space` | Add blank line above/below |

---

## Command Mode (`:`)

Typable commands accessed with `:`.

| Command | Action |
|---------|--------|
| `:w` / `:write` | Save |
| `:q` / `:quit` | Close buffer |
| `:wq` | Save and quit |
| `:q!` | Force quit without saving |
| `:o <path>` / `:open` | Open file |
| `:bn` / `:buffer-next` | Next buffer |
| `:bp` / `:buffer-prev` | Previous buffer |
| `:bc` / `:buffer-close` | Close buffer |
| `:theme <name>` | Change theme |
| `:set <opt> <val>` | Set option |
| `:lang <lang>` | Set syntax language |
| `:format` | Format file (LSP) |
| `:lsp-restart` | Restart LSP |
| `:pipe <cmd>` | Pipe selection through shell |
| `:insert-output <cmd>` | Insert command output |
| `:append-output <cmd>` | Append command output |

---

## Keys Available for Custom Bindings

In normal mode, these keys have no default Helix binding and are safe to use:

`H`, `J`, `K`, `L` — (note: K shows hover in some versions, verify)
`Q` — replay macro (actually taken)
`0` — go to line start (taken in some versions — verify)

**Generally safe for custom minor modes** (as prefix keys):
No single-letter prefix keys are truly "free" in Helix normal mode — all commonly-used
letters are taken. For custom applications, the safest approach is:
- Define a custom "application mode" triggered by a unique prefix
- Use `Tab` as an application-specific prefix (often unmapped or only used for completion in insert)
- Use `Enter` / `Ret` for confirmation rather than a mode prefix
- Reserve entirely new keys for your app's normal mode rather than adding prefixes
