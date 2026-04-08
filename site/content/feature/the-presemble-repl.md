# The Presemble REPL

Evaluate expressions against your live site content from Calva, CIDER, or the command line.

`presemble nrepl site/` starts an nREPL server. Any nREPL-compatible client can connect: Calva in VS Code, CIDER in Emacs, or `rep` from the command line. Once connected, you evaluate Presemble Lisp expressions against the live content graph.

----

### Connecting

Start the server:

```
presemble nrepl site/
```

Connect with `rep` (command-line nREPL client):

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

Keywords act as functions: `:title item` extracts the `:title` field from `item`.

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
