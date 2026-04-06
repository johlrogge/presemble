# The Dream of a Semantic Site Generator

You may wonder why on earth another site generator is really needed. There are many, many alternatives, and I've tried a lot of them.

In this post I'll try to explain why I have not been able to let go of this idea for over two decades. Yes, you read that right, even before site generators were "a thing".


[Joakim Ohlrogge](/author/johlrogge)

----

### There are 2 hard problems in software...

You know how the _original_ joke goes:

> there are two hard problems in software: naming things, cache invalidation and off by one errors.

I was working as a contractor for a customer that had invested heavily in a platform called Broadvision. The product promised personalization. What they delivered: a patchwork of expensive products that did not perform and weren't even thread safe. Our team spent a lot of time trying to deliver anyway, and I was young enough to clock 36-hour working days more than once.

Broadvision claimed to do caching really well. I'm thankful to them for teaching me a lot about what not to do. And we started thinking: what if we pushed content to the cache proactively instead of reactively? What if each page knew which assets it needed and a tool could deliver them automatically?

Incidentally, a few years after that I worked for a large phone manufacturer and their site. They actually had something that resembled a static page generator, but it was the inverse, it was request driven and a glorified cache. Another cool thing: they used Akamai's content delivery network and edge computing which would actually benefit a lot from cache predictability. Again I started thinking of how to reverse the process.

### Trying and failing

I have made many attempts over the years to get this generator right. I have started pretty naievly many times and learned each time. This time I think I have nailed it. And I'll try to explain why this time is different, and why getting the model right matters.

#### Homoiconicity

Homoiconicity is a fancy word for a property that, among others, Lisp and its dialiects display. What this means for a programming language is roughly that the code is represented in the same way as data which makes meta programming easier. You can use the language itself modify it's code since the code is expressed in the data structure of the language.

Homoiconicity is merely an inspiration for presemble and what is important for presemble is that the text the different entities are expressed as are merely a serialization form. The primary representation of an entity (template, schema, css, and markdown) are the nodes produced by parseing the content. The DOM-tree is primary and the textual representation secondary (so don't be surprised by a reformat when you save your "text").

#### Well formed, templates

My first attempt was _pick a template language and move on_. Then I remembered [templistic](https://templistic.vercel.app/), that two of my collegues at Agical, Olle Wreede and Daniel Brolund invented. Templistic runs as javascript in the browser, but I cannot emphasize how much of an inspiration templistic was for what templates in Presemble looks like.

Remember homoicnonicity? Since we only care about the DOM of the templates we can use `"HTML"` and `EDN`, (note the quotes around _HTML_), we don't follow the HTML specification in every respect, for instance, we allow `<self-closing-tags />`. The only two representations we support at the moment is HTML and EDN since they allow _mixed content_ which json for instance does not.

Well, being able to switch representations is not new, it would be easy to support many different template engines. But that we work at the DOM-level changes the game completely, well, not really, we already work with the dom in the browser, so why not use the same patterns serverside? One particularily interesting aspect of that is that it is _easy_ to extract references to other pages, images and stylsheets for instace from the node tree. Because our templates are not just text with holes anymore: [Templates Are Data](/feature/templates-are-data).

So now we have a few types of node trees: template-trees, content (markdown) trees that we have mentioned. While they have different meanings they are in a sense just nodes that can be traversed and transformed.

#### Making it semantic
In the same way templates are not "just text with holes anymore", I did not want content to be "just documents". I wanted Presemble to understand the _structure_ of the content, not just the text.

I have failed at this before. I tend to derail into semantic web technologies — taxonomies, ontologies, RDF schemas — and before long my original idea is swallowed by a linguistic problem I've convinced myself I have to solve first.

I needed something simpler. A markdown schema: not as expressive as the semantic web, but it can say things like "this document represents an author with a name and a biography" — and it can enforce that at build time.

The semanticness also adds structure and I can spec the structure of my site with placeholders, that look like markdown, with some contraints like `occurs exactly once`. I can say that a blog post like this one, should link to an author, start with a capitalized title and the template can reference the linked author and the title for instance.

If the post does not conform, it can't be published.

#### Separate content and presentation

When combining schemas and markdown content, something pretty remarkable happens: you have a content model that is completely independent of presentation. What do I mean by that?

Most sites have a header, perhaps navigation, a body, a footer, a sidebar etc. What decides what content goes into all that extra page structure? Usually it is the template the page is rendered with. The template selects content and presents it, often even contains logig for showing/hideing/highlighting specific parts of it.

But when the content is semantic it can make a lot of those decisions. For instance: I could make a markdown document that links to another document, like an author. Or a collectio of documents, like a list of products. It could add some editorial content like a paragraph and a headline to this without even thinking about how this will be presented. You have a navigatable _content graph_.

What the template adds to this is _how to present_ the content visually.

#### Pure templates

The first version of presemble pulled in content via templates, but in a round about way I started thinking about _pure templates_. What if a template was just a function that takes a node tree in and produces a new node tree? What would have to be true for that to be possible and what would pure mean?

The obvious first: no side effects, that was already true. But I realized another trait: no content selection! The template can't sideload any content in addition to what it got passed as input. And the afore mentioned content separation fell out of that.

The templates receive all it's data as input and produces it's output based on that.  The template _can_ apply other _pure_ templates and still be pure:

```clojure
;; A local template definition — just a named function
[:template {:presemble/define "body"}
  [:main {:class "post"}
    [:article
      [:presemble/insert {:data "input.title" :as "h1"}]
      [:presemble/insert {:data "input.summary" :as "p"}]
      [:div {:class "byline"}
        "By " [:presemble/insert {:data "input.author" :as "a"}]]
      [:presemble/insert {:data "input.body"}]]]]

;; The page template — compose header, body, and footer
;; Each receives the same input. Outputs are concatenated.
[:html {:lang "en"}
  [:head
    [:title "Presemble"]
    [:link {:rel "stylesheet" :href "/assets/style.css"}]]
  [:body
    (juxt header self/body footer)]]
```
The `body` definition is a pure function: it receives a content tree as `input` and produces a DOM tree. It cannot select content, it cannot reach outside its input. The `juxt` composes three such functions — header, body, footer — applying each to the same input and concatenating their DOM output.

#### The content graph

When a blog post links to an author, that link is not just a URL string. It is a typed edge in a graph. The publisher resolves it at build time: the author document's name, biography, and portrait become available to the template that renders the post. If the author does not exist, the build fails — not with a 404 at request time, but with a clear error naming the missing reference.

This turns the site into a navigatable graph of interconnected content. A homepage can assemble itself from that graph: pull in the five most recent posts, three featured articles, the site metadata — all via expressions in the content file, not in the template. The content decides what appears. The template decides how it looks.

#### Your editor knows your content

Presemble ships an LSP server. Point your editor at it and your content files become first-class citizens: completions for every slot the schema declares, diagnostics for every violation, hover documentation showing the schema constraints, and go-to-definition that jumps from a content reference to the referenced document.

This is not syntax highlighting. The editor understands the _meaning_ of your content — which fields exist, which are missing, which values are invalid. The feedback loop is immediate: type a wrong value and the diagnostic appears before you finish the line.

#### Instant feedback

Run `presemble serve` and a local server starts watching your files. Save a content file and the browser reloads — navigating directly to the page you changed, not the page you were looking at. Move your cursor in the editor and the browser scrolls to follow. Missing content renders as warm placeholders that show the layout as it will appear when the content is filled in.

The page always renders. There are no blank sections, no crashes, no "content not found" messages during development. The placeholders are a development aid — at publish time, missing required fields are still build errors.

#### Compile-time safety

If the content does not satisfy the schema, the site does not build. This is not a warning you can ignore — it is a hard gate. Every constraint is checked before any output is written: cardinality, capitalization, link targets, heading levels. The error message names the file, the slot, and the constraint that failed.

This means: if the site builds, it is correct by construction. No runtime surprises, no broken deploys, no "I forgot to fill in the author field" discovered by a reader.

#### Editorial collaboration

The suggestion system is designed for human editors first. An editor proposes a change to a title, a summary, a paragraph — the suggestion appears in the author's editor as a diagnostic. Accept or reject, inline, with one keystroke.

The same protocol works for AI. Claude reads the content and the schema, understands the constraints, and proposes improvements through the same suggestion mechanism. The author cannot tell — and does not need to tell — whether a suggestion came from a colleague or from Claude. The author is always in charge.



