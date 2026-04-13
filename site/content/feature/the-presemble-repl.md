# The Presemble REPL

Evaluate expressions against your live site content — from the terminal, from Calva, CIDER, or the command line.

`presemble repl` opens a full-screen terminal REPL. `presemble nrepl site/` starts an nREPL server for Calva, CIDER, or `rep`. Both evaluate Presemble Lisp expressions against the live content graph.

----

### TUI REPL

```
presemble repl
presemble repl --port 1667
```

The TUI REPL runs in the terminal with three panels: an output history, a doc panel, and an input editor.

**Auto-discovery:** with no flag, the REPL walks the current directory and its parents looking for a `.nrepl-port` file written by a running conductor. If one is found it connects automatically. If no conductor is running it starts in standalone mode, where language primitives and prelude functions work fully and site-specific operations return informative errors.

**Key bindings:**

| Key | Action |
|---|---|
| Enter | Eval when delimiters are balanced; insert newline otherwise |
| Ctrl+J | Force-eval regardless of balance |
| Ctrl+O | Force-insert newline |
| Tab | Completion popup |
| Ctrl+D | Quit |
| Ctrl+L | Clear output |

**EDN syntax highlighting** colours keywords, strings, numbers, brackets, and comments. The **completion popup** shows matching symbols with inline doc hints. The **doc panel** shows arglists and the full doc string for the symbol under the cursor. **Command history** navigates with Up/Down.

### nREPL

```
presemble nrepl site/
```

Starts an nREPL server. Connect with `rep` (command-line nREPL client):

```
rep '(->> :post (sort-by :published :desc) (take 3))'
```

In Calva: run "Connect to a Running REPL Server" and select nREPL. In CIDER: `M-x cider-connect`.

### Presemble Lisp

Presemble Lisp is a small Lisp built into the publisher. It has four components: a reader (EDN-based), a macro expander, an evaluator, and a set of built-in functions. Expressions are written in EDN and evaluate against the live site graph.

The language is purposely minimal: it covers the operations content assembly actually needs — filtering, sorting, taking, and threading — without becoming a general-purpose programming language.

**Threading macros:**

```clojure
(->> :post (sort-by :published :desc) (take 5))
```

Threads the value through each form as the last argument. This is the primary idiom for collection assembly.

```clojure
(-> input :title upcase)
```

Threads as the first argument. Used for single-value transforms.

**Built-in functions (selection):**

| Function | Effect |
|---|---|
| `sort-by` | Sort a collection by a field |
| `take` | Keep the first N items |
| `drop` | Skip the first N items |
| `filter` | Keep items matching a predicate |
| `map` | Apply a function to each item |
| `count` | Number of items |
| `first` / `last` | First or last item |
| `upcase` / `downcase` | String transforms |
| `str` | Concatenate strings |
| `refs-to` | All edges pointing to a given URL: `(refs-to "/author/alice")` |
| `refs-from` | All edges originating from a given URL: `(refs-from "/post/hello")` |

Keywords act as functions: `:title item` extracts the `:title` field from `item`.

Edge records returned by `refs-to` and `refs-from` expose `:source` and `:target` keys. Use them to explore the site's link graph from the REPL:

```clojure
(->> (refs-to "/author/alice") (map :source))
```

### Link expressions in content

Content files can include link expressions that assemble collections inline:

```markdown
[]((->> :post (sort-by :published :desc) (take 5)))
```

The expression is evaluated at build time. The result is a typed list that satisfies the collection schema for that type. The template receives the assembled list and presents it without any iteration logic of its own.

This is the content-as-assembly model: the homepage content file decides which collections appear and in what order; the template decides how they look.

### Template composition with juxt

Templates are function files. The composition expression at the bottom of each template file applies its inputs through `juxt` or pipe:

```clojure
((juxt header self/body footer) input)
```

`juxt` fans the same input to multiple templates and concatenates their DOM outputs in order. `header`, `self/body`, and `footer` all receive the full content tree; their outputs are assembled into a single page DOM.

Local definitions at the top of a template file work like `let` bindings — named fragments that are reused within the file.

### Smoke testing

The `tools/smoketest.sh` script exercises the full workflow end-to-end using `curl` and `rep`. It starts the server, creates content via the conductor API, evaluates REPL expressions against the live graph, and verifies the output pages. Use it to confirm a build environment is working.
