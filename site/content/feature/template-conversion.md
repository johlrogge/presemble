# Template Conversion

Switch between HTML and Hiccup template syntax in one command.

`presemble convert` translates template files between HTML and Hiccup (EDN) surface syntax without loss. The underlying DOM tree is format-agnostic — conversion is a round-trip parse and serialize.

----

### HTML to Hiccup

```
presemble convert templates/post/item.html
```

Produces `templates/post/item.hiccup`:

```clojure
[:html {:lang "en"}
 [:body
  [:presemble/insert {:data "input.title" :as "h1"}]
  [:presemble/insert {:data "input.body"}]]]
```

### Hiccup to HTML

```
presemble convert templates/post/item.hiccup
```

Produces `templates/post/item.html`:

```html
<html lang="en">
  <body>
    <presemble:insert data="input.title" as="h1" />
    <presemble:insert data="input.body" />
  </body>
</html>
```

### When to use which format

HTML is the default. It is what browsers and most editors understand natively. Hiccup is a Clojure data literal — useful in REPL-driven workflows, editor tooling, and anywhere you want to treat the template as data rather than markup. Both formats are first-class: the publisher accepts either without configuration.

### Hiccup comments

Hiccup templates support line comments with `;`:

```clojure
; This section renders the page title
[:presemble/insert {:data "input.title" :as "h1"}]
```

Comments are stripped at parse time and do not appear in output.
