# Helix & Kakoune Philosophy

## The Core Inversion: Selection-First (Object→Verb)

**Vim grammar**: `verb → object` — specify the action, then scope
- `dw` = "delete" → "word"
- Problem: you edit blind; you don't see what will be affected until after

**Helix/Kakoune grammar**: `object → verb` — select first, then act
- `wd` = select word → delete
- Advantage: full visual feedback before any destructive action

This inversion is not cosmetic. It changes how you think about editing:
- You always know exactly what will be changed
- Mistakes are caught visually, not discovered after undo
- Multi-selection becomes natural, not an add-on

## A Cursor Is Just a Selection

In Helix, there is no separate "cursor" concept — a cursor is a single-width selection.
Every selection has an anchor and a cursor point.

- Normal movement: moves both anchor and cursor together
- Extend mode (`v`): keeps anchor fixed, moves cursor only
- This unifies visual selection and cursor movement into one model

## Kakoune's Contributions (Adopted by Helix)

Kakoune pioneered the ideas Helix builds on:

| Concept | What it means |
|---------|--------------|
| Selection-first grammar | Object then verb, always |
| Multiple selections as primary | N simultaneous cursors, not a workaround |
| Real-time regex feedback | See matches as you type the pattern |
| `s` for split-selection | Split current selection by regex into N sub-selections |
| `<a-k>` filter | Keep only selections matching a pattern |
| `&` alignment | Align multiple cursors to same column |
| Text objects | `mi(`/`ma(` for inside/around pairs |

## How Helix Differs from Kakoune

| Aspect | Kakoune | Helix |
|--------|---------|-------|
| Window splits | None | Built-in (Ctrl-w) |
| LSP integration | External | Native |
| Tree-sitter | External | Native |
| DAP debugging | External | Native |
| Philosophy | Unix composability | Integration/batteries-included |
| Space/leader mode | Not present | Sticky leader (Space) |

## Kakoune Design Maxims

From the Kakoune design document:

1. **Correctness over shortcuts** — real-time feedback prevents wrong edits
2. **Composability** — small primitives combine into complex operations
3. **Interactivity** — every action shows immediate visual result
4. **Keystroke parity** — object-first is competitive with verb-first in keystroke count,
   while being more predictable

## Implications for Non-Editor TUIs

When designing keymaps for domain-specific TUIs (music players, file managers, etc.):

- **If your UI has a list, it has selections** — design for selection-first
- **Real-time feedback is non-negotiable** — highlight selection before acting
- **Multiple selection multiplies value** — batch operations are free if the model supports it
- **Modes reduce cognitive load** — fewer things to remember per context
- **Familiar keys reduce friction** — if `d` means "delete" in every modal UI, users win
