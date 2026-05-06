# ADR-042: URL-fragment mode encoding for editorial lenses

## Status

Superseded by [ADR-044](044-schema-mode-uses-canonical-urls.md). Schema mode
now uses canonical `/_schema/` URLs rather than the `#_schema` fragment;
edit and suggest modes retain their hash form.

## Context

Presemble has three editorial browser modes today: view, edit, and suggest. Phase D adds **structure** as a fourth mode, which lets authors browse and inspect the schema driving any given page. While scoping Phase D we redesigned how mode is encoded so that all four modes are handled uniformly.

Today, edit and suggest modes live in `presemble-mode` sessionStorage rather than in the URL. They are not bookmarkable or shareable: a collaborator who opens a fresh tab, or follows a link, always lands in view mode regardless of what the sender intended. Adding structure mode as a fourth sessionStorage value would extend that limitation rather than fix it.

The hash portion of a URL (the fragment) is the conventional place for "same resource, different lens." It is not sent to the server, which keeps the server simple, and it is visible in the address bar, which makes mode transparent and shareable.

## Decision

Editorial mode is encoded in the URL fragment using an underscore-prefix convention that matches the existing `_presemble/*` URL-path namespace for internal resources:

| Hash | Mode |
|------|------|
| `#_schema` | Structure mode — browse and inspect the schema for the current page |
| `#_edit` | Edit mode — inline editorial editing |
| `#_suggest` | Suggest mode — tracked-changes suggestion editing |
| *(no hash)* | View mode — default, read-only rendered page |

The server is mode-agnostic. It serves content the same way regardless of which hash is present. The URL path identifies the *resource*; the fragment identifies the *lens* through which the browser presents it. Mode is realized in the browser by `inject.js`, which reads `location.hash` on page load and on `hashchange` events, and activates the appropriate client-side behaviour.

Schema rendering (structure mode) is handled at request time via a new conductor render endpoint:

```
GET /_presemble/render?path=<path>&mode=<view|schema>
→ 200 text/html   (rendered page body)
→ 404             (no rendering available for that path–mode combination)
```

When `inject.js` detects `#_schema` it issues a fetch to this endpoint and replaces the page body with the schema view. This endpoint is also the natural seam for future remote-conductor work (jack-in): a remote conductor would expose the same shape of endpoint, so client code needs no changes when editor collaboration moves off-localhost.

`presemble-mode` sessionStorage is retired in Wave 3 (T11). The hash becomes the single source of truth for mode. The absence of a hash means view mode.

## Why

Encoding mode in the URL fragment rather than sessionStorage gives several concrete benefits. All three editorial modes become bookmarkable and shareable without any special infrastructure — a contributor can paste a URL ending in `#_edit` and the recipient lands directly in edit mode. Mode becomes per-tab-per-URL rather than session-wide, so opening a new tab no longer inherits a stale mode from another tab. The server stays unchanged because the fragment is client-only; no server-side routing or mode-aware content negotiation is required.

The underscore prefix (`#_schema`, `#_edit`, `#_suggest`) is chosen deliberately to avoid collision with author-defined heading anchors. A heading literally titled "Schema" would produce a fragment of `#schema`; `#_schema` is distinct. The prefix mirrors the existing `_presemble/` URL path convention for Presemble-internal routes, making the internal namespace recognisable to anyone already familiar with the project.

The conductor render endpoint (`/_presemble/render`) is the seam that cleanly separates the browser client from the rendering engine. Today the conductor is local; in the jack-in model it will be remote. Because the endpoint shape is stable, the client-side code requires no changes when that transition happens.

## Alternatives considered

1. **Reserved path segment** (e.g., `/blog/_schema/item`) — rejected. It adds visual noise to URLs, requires a new URL-path namespace in the server router, and would not unify with edit and suggest modes, which would still use sessionStorage.

2. **Query parameter** (e.g., `?mode=structure`) — rejected. Less idiomatic than a fragment for "same resource, different view." Query parameters conventionally signal "different request to the server," whereas a fragment signals "same resource, client-side interpretation." Using a query parameter would also require more careful server-side filtering to avoid cache pollution.

3. **Cookie or header-based mode signal** — rejected. The URL would be ambiguous when shared out of context: a recipient would not enter the intended mode without sharing the cookie or session state. This defeats the shareability goal entirely.

## Consequences

**Positive:**

- All three editorial modes (edit, suggest, structure) are now URL-bookmarkable and shareable. Sending a link to a specific mode becomes straightforward.
- Mode is scoped to a tab and URL, not the entire session. Stale-mode bleed between tabs is eliminated.
- The server stays simple: no mode-specific routing or content negotiation.
- The conductor render endpoint (`/_presemble/render?path=…&mode=…`) is a well-defined seam for the future remote-conductor (jack-in) work.

**Trade-offs:**

- A direct deep-link to a `#_schema` URL on a path that has content will briefly show the content page before JavaScript detects the hash and swaps to the schema view via the render endpoint. This flash is acceptable for Slice 1; a no-flash solution (server-side redirect or preloaded schema HTML) can be deferred until user feedback warrants it.
- Leaving structure mode reloads the page to restore the content view from a fresh GET, which discards client-side state such as scroll position. Acceptable for Slice 1; revisit if user experience feedback demands it.

## Cross-references

- `docs/adr/031-conductor-as-sole-authority.md` — confirms the server is mode-agnostic; conductor is the authority for rendering.
- Future remote-conductor work (jack-in) — the `/_presemble/render` endpoint is the integration seam.
- Memory notes: `project_browser_editing.md`, `project_jack_in.md`, `project_structure_mode.md`.
