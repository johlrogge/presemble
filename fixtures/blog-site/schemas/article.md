# Your blog post title {#title}
occurs
: exactly once
content
: capitalized

paragraphs [1..3] {#summary}
occurs
: at least once

[<name>](/authors/<name>) {#author}
occurs
: exactly once

![cover image description](images/*.(jpg|jpeg|png|webp)) {#cover}
orientation
: landscape
alt
: required

----

Body content. Headings H3–H6 only (H1 and H2 are reserved for the template).
headings
: h3..h6
