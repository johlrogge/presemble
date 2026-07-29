# ADR-054: V0 editorial-plane auth boundary

## Status
Accepted

## Decision

`presemble serve` now enforces an **authenticated boundary around the editorial plane**. This is the first shipped piece of hosted Presemble (ADR-051 / M11 minimum, ADR-053 Task 1) and the prerequisite for ever handing a real invitee a link.

**The whole `/_presemble/*` namespace is editorial and requires a verified session.** Every editorial route — all mutations (`edit`, `edit-body`, `apply`, `create-content`, `ned-suggestions[/accept|/reject]`, `save-all`, `scaffold`, `style-preview`) and every editorial *read* (`render`, `grammar`, `schemas`, `links`, `dirty-buffers`, `templates`, `font-moods`, `palette-types`, `schema-for`, `page-for`, the suggestion lists, and the LSP websocket) — takes an authenticated `Session`. The `/_schema/...` interception in the published-file handler is editorial too and is gated. The default is **gated**: a new `/_presemble` route is authenticated unless deliberately exempted.

**Published output stays public.** Serving the built site (the fallback file handler) requires no session — a published Presemble site is a public website (ADR-051: the boundary is the editorial plane, not readership). The **live-reload websocket (`/_presemble/ws`) is a deliberate public exception**: it is injected into published preview pages and carries only broadcast reload pings (page paths), never unpublished content; gating it would break live reload for the public preview.

**V0 is authentication, not authorization.** Any valid session grants full editorial access to whatever site the `serve` process is serving. This is sufficient because one conductor serves one account's workspace (ADR-035/053, one conductor per account); per-principal / per-site authorization is deferred to V1+ and must not be faked here.

**The boundary is type-enforced, not a per-route check.** Authentication lives in the `accounts` component: a `Session` newtype with no public constructor from untrusted input, produced only by `SessionStore::verify` / `verify_str`, and an axum `FromRequestParts` extractor that returns `401` before any handler body runs. An editorial handler names `Session` in its signature; a handler that omits it cannot obtain one, and the editorial routes are additionally isolated on a nested router. "Make illegal states unrepresentable" applied to a security boundary: an ungated editorial handler does not compile. V0 ships an HMAC-SHA256 `DevSessionStore` with a `--dev-session` local-login flow; V1 swaps in a magic-link `SessionStore` implementing the same trait, with no handler changes.

## Why

1. **A single shared password could not survive a real link.** Before V0 the editorial plane was gated only by one shared Caddy `editor` password — world-writable to anyone who had it, with no per-user suggestion authorship. Accounts make an app-level session boundary mandatory.
2. **"You should not see what has not been published to you."** The core security principle (architect skill) means editorial *reads*, not just writes, must be gated — hence the whole-`/_presemble` rule rather than "gate POSTs only."
3. **Type-enforcement beats vigilance.** A `Session`-typed extractor makes the check un-forgettable on any handler that carries it; a runtime `if unauthorized` sprinkled per route is exactly what gets missed on the next added endpoint.
4. **Authn-first is honest for the alpha.** One conductor = one account's workspace, so authentication is the boundary that matters now; authorization is real work that should not be stubbed.

## Alternatives considered

- **Gate only mutations (POSTs), leave GET reads public** — rejected; editorial GETs expose unpublished/schema/dirty-buffer state, which violates the "not published to you" principle.
- **Keep the shared Caddy password** — rejected; not per-user, trivially shared, no authorship, unfit for invitees.
- **Runtime prefix-match middleware as the primary gate** — rejected as the load-bearing mechanism; a string-prefix `if` is the forgettable check the type boundary exists to replace. (A nested router is used as an audit aid, not the guarantee.)
- **Gate the live-reload websocket too** — rejected; it breaks public preview reload and carries no unpublished content. Documented as a bounded, accepted exception.
- **Do per-site authorization now** — deferred; unnecessary while one conductor serves one account, and real enough to deserve its own design (V1+).

## Consequences

- The `accounts` component owns authentication (`Session`/`SessionToken`/`Principal`, `SessionStore`, the extractor); it is the seam V1's magic-link store and `Account` types grow into.
- Every future `/_presemble` route must take `Session` (gated by default); exempting one is a deliberate, reviewable act. The public exceptions are exactly: published file serving and the live-reload ws.
- The Caddy `editor` basic_auth is now redundant for editorial endpoints and can be retired once V0 is verified in production (a separate devops follow-up; do not remove it in the same change that adds the app gate).
- **Known V0 gaps (accepted):** authn-only (no per-principal authorization); the live-reload ws is public; the `--dev-session` flow prints a token usable via a URL query param (dev-only, localhost convenience).
- Builds on ADR-031 (conductor as sole authority), ADR-035 (one conductor serves one account's workspace), ADR-051 (accounts / the editorial-plane-not-readership boundary), ADR-053 (this is its Task 1). Shipped in commit `c6dbf10`.
