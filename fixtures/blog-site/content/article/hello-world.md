# Hello, World: Getting Started With Presemble

Presemble is a site publisher built for editorial teams who care about content structure and
semantic safety. This post walks you through the core ideas behind the project.

In this article you will learn what presemble is, why we built it, and how the annotated markdown
schema format keeps your content well-formed at build time.

You will also see how named slots and document grammars give templates reliable, queryable
fields without any runtime surprises.

[Jo Hlrogge](/authors/johlrogge)

![A landscape photo of a desk with a laptop and notebook open side by side](images/cover.jpg)

----

### What Is Presemble?

Presemble combines a static site publisher with an editorial collaboration layer. Content schemas
are written in plain markdown, so authors can read them without any training.

#### Why Document Grammars?

Most schema formats describe a bag of fields. Presemble instead describes the expected sequence
of structural elements in a document — heading, image, paragraph, body — with each position
named and constrained.

##### Named Slots in Practice

Every named slot becomes a queryable field in templates. A template can reference
`${article:title}` or `${article:cover}` and the publisher guarantees those values exist before
the build completes.

###### Constraint Vocabulary

The constraint vocabulary covers the most common needs: `occurs`, `content`, `orientation`,
`alt`, and `headings`. This vocabulary will grow as the experiment matures.
