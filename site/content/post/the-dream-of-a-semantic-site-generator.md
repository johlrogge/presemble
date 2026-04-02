# The dream of a semantic site generator

You may wonder why on earth another site generator is really needed. There are many, many alternatives, and I've tried a lot of them.

In this post I'll try to explain why I have not been able to let go of this idea for over two decades. Yes, you read that right, even before site generators were "a thing".

[Joakim Ohlrogge](/author/johlrogge)

----

### There are 2 hard problems in software...

You know how the original joke goes:

> there are two hard problems in software: naming things, cache invalidation and off by one errors.

I was working as a contractor for a customer that does not need to be named. They had invested heavily in a platform: broadvision. I would be extremely surprised if this product is not long dead and even if it's not I think this post that almost noone will read is a small payback for the immense suffering their shitty product caused me.

The product promised personalization. What they delivered: a patchwork of expensive products that did not perform and wasn't even thread safe. Our team spent a lot of time trying to deliver anyway and I was young and stupid and clocked 36h working days more than once.

Broadvision claimed to do cacheing really well and I am very thankful to them for learning a lot of things about what not to do (they were better at making their customers pay cash...). And we started thinking: what if we would push content to the cache when needed instead? And what if each page knew which assets it needed and those were automatically delivered by some tool?

Incidentally, a few years after that I worked for a large phone manufacturer and their site. They actually had something that resembled a static page generator, but it was the inverse, it was request driven and a glorified cache. Another cool thing: they used akamai's content delivery network and edge computing which would actually benefit a lot from cache predictability. Again I started thinking of how to reverse the process.

### Trying and failing

I have made many attempts over the years to get this generator right. I have started pretty naievly many times and learned each time. This time I think I have nailed it. And I'll try to explain why this time is different, and why getting the model right matters.

#### Homoiconicity

Homoiconicity is a fancy word for a property that, among others, Lisp and its dialiects display. What this means for a programming language is roughly that the code is represented in the same way as data which makes meta programming easier. You can use the language itself modify it's code since the code is expressed in the data structure of the language.

Homoiconicity is merely an inspiration for presemble and what is important for presemble is that the text the different entities are expressed as are merely a serialization form. The primary representation of an entity (template, schema, css, and markdown) are the nodes producesd by parseing the content. The dom-tree is primary and the textual representation secondary (so don't be surprised by a reformat when you save your "text").

#### Well formed templates

My first attempt was _pick a template language and move on_. Then I remembered [templistic](https://templistic.vercel.app/), that two of my collegues at Agical, Olle Wreede and Daniel Brolund invented. Templistic runs as javascript in the browser, but I cannot emphasize how much of an inspiration templistic was for what templates in Presemble looks like.

Remember homoicnonicity? Since we only care about the DOM of the templates we can use `"HTML"` and `EDN`, (note the quotes around _HTML_), we don't follow the HTML specification in every respect, for instance, we allow `<self-closing-tags />`. The only two representations we support at the moment is HTML and EDN since they allow _mixed content_ which json for instance does not.

Well, being able to switch representations is not new, it would be easy to support many different template engines. But that we work at the DOM-level changes the game completely, well, not really, we already work with the dom in the browser, so why not use the same patterns serverside? One particularily interesting aspect of that is that it is _easy_ to extract references to other pages, images and stylsheets for instace from the node tree. Because our templates are not just text with holes anymore: [Templates Are Data](/feature/templates-are-data).

So now we have a few types of node trees: template-trees, content (markdown) trees that we have mentioned. While they have different meanings they are in a sense just nodes that can be traversed and transformed.

#### Making it semantic

In the same way templates are not "just text with holes anymore", I did not want content to be "just documents". I wanted presemble to understand a bit more about the content than that. It may not be completely obvious why but I hope to demonstrate why it may be useful. But first a bit about how I have failed in this regard before: I tend to derail with semantic web technologies, and before long I have lost sight of the problem I'm trying to solve and fumble around in taxonomies and onthologies, and I'm completely lost in RDFs, RDF schemas and whatever all that is called: my original idea is swallowed by a huge linguistic problem that I have convinced myself that I have to solve first.

I need something simpler. So I came up with a markdown schema, id is not as feature rich as the semantic web perhaps but can still express things like "this document represents an author has a name, a biography..."

The semanticness also adds structure and I can spec the structure of my site with placeholders, that look like markdown, with some contraints like "occurs exactly once". I can say that a blog post like this one, should link to an author, start with a capitalized title and the template can reference the linked author and the title for instance.

If the post does not conform, it can't be published.
