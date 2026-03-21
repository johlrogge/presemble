# VISION.md — Presemble

## What is Presemble?

Presemble is a site publisher built around the idea that content creation and site design are
fundamentally different activities — and that treating them the same way causes friction, delays,
and preventable failures.

The name: *pre-* (the upstream collaborative phase) + *semble* (ensemble — the gathering).

## The Problem

Static site toolchains today are assembled from parts that each bring the wrong trade-offs:

- **Google Docs** gives real-time collaboration and commenting, but wraps it in WYSIWYG noise
  and lives outside the publish pipeline.
- **Git** is the right home for code, templates, and design — but editorial collaboration through
  branches, pull requests, and merge conflicts is hostile to writers. A blog author should not
  need to care that other blog posts changed.
- **Scheduling** is bolted on as an afterthought, if it exists at all.

Presemble replaces this patchwork with an integrated system where each concern lives in the
right place.

## Two Modes of a Site

A site has two distinct modes of work:

- **Content creation** — independent per piece, multiplayer, real-time. Needs fast collaboration
  (commenting, suggestions, track changes) without push/pull/merge ceremony.
- **Design and layout** — affects the whole site, code-like, versioned. Git is the right home.

Presemble keeps these separate where they diverge and connected where they must agree.

## Architecture

Three components, each with a clear role:

- **Git repo**: Source of truth for schemas, templates, and design. Also holds a pointer to the
  content system location.
- **Content system**: A separate service for multiplayer real-time editorial work. It knows the
  schemas from git and stores and serves content.
- **Publisher**: Pulls schemas and templates from git, pulls content from the content system,
  and compiles the site. The publisher is a hard gate — it will not publish if content does not
  compile against its schemas.

The analogy: the publisher is `rustc` (the compiler — a hard gate at publish time). The content
editor is `rust-analyzer` (real-time guidance as you write, surfacing problems before you hit
the gate).

## Semantic Safety

Schemas are not optional metadata. They are a gate in the editorial pipeline.

- Content cannot advance or publish if it does not satisfy its schema.
- Validation covers structure (heading levels, required fields) and cross-content references
  ("the referenced author has no bio" blocks the article).
- The content editor provides real-time schema feedback while writing — the same way a language
  server catches errors before you compile.

### Two Kinds of Content

- **Document-shaped**: Articles, blog posts — markdown with structural validation.
- **Data-shaped**: Typed records like a tea catalog — origins, variety, color, price per unit.
  More like database records than documents.

Both are schema-driven. Both go through the same validation gate.

## Time as a First-Class Concept

Time is not a flag on a content item. It is a dimension of the site.

- The publisher maintains a living timetable of future publish events.
- Content saves push events into the timetable ("happy new year" banner goes live Jan 1 at 00:00).
- The content system can signal the publisher to wake up earlier when the schedule changes.
- **Timeline UI**: A visual scrubber — click any date, see the site as it will exist at that moment.
- Time-travel preview is a natural consequence of making time first-class, not a special feature.

### Publish Triggers

Dual: **clock-driven** (timetable) and **event-driven** (git push or content save).

## User Hierarchy

Presemble supports three layers of engagement, each building on the last:

1. **Browser-first**: Zero install. Anyone can write a blog post. No knowledge of the publisher
   required.
2. **In-browser editing on local/stage serve**: Click on the page you are viewing, edit it right
   there. No indirection between what you see and what file it maps to.
3. **Editor + LSP (e.g., Helix)**: Power-user layer. Comments surface as diagnostics.
   `presemble serve` creates a bidirectional IPC bridge between browser and editor. Suggest
   changes from the editor push to the content system.

## The Unifying Theme: Shortening Distances

Every design decision in Presemble serves the same principle — shorten the distance:

- Between writing and publishing
- Between intent and result
- Between "what it looks like" and "what it will look like when published"
- Between "I think this will work" and knowing it will not fail at publish time
- Between what you see in a browser and what file that corresponds to

## Security

Security is a first-class concern, not an afterthought. Future and staged content must be
protected — time-travel preview and access to unpublished content require authorization, not
just authentication. The specific mechanisms (OAuth flows, role-based access, content system
auth) will be designed as the system takes shape, with dedicated security review built into
the development process.

The principle: **you should not be able to see what has not been published to you.**

## Values

- **Writers are not developers.** The default experience requires no tooling knowledge.
- **Safety is structural, not procedural.** Schemas enforce correctness; humans should not
  have to remember to check.
- **Collaboration is real-time, not transactional.** Content editing should feel like writing
  together, not exchanging patches.
- **Time is visible.** The future state of the site should be as inspectable as its current state.
- **Security is not an afterthought.** Access control is designed in, not bolted on.
