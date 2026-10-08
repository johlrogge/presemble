# ADR-054: V0 editorial-plane auth boundary

## Status
Accepted

## Decision

`presemble serve` now enforces an **authenticated boundary around the editorial plane**. This is the first shipped piece of hosted Presemble (ADR-051 / M11 minimum, ADR-053 Task 1) and the prerequisite for ever handing a real invitee a link.

**The whole `/_presemble/*` namespace is editorial and requires a verified session.** Every editorial route — all mutations (`edit`, `edit-body`, `apply`, `create-content`, `ned-suggestions[/accept|/reject]`, `save-all`, `scaffold`, `style-preview`) and every editorial *read* (`render`, `grammar`, `schemas`, `links`, `dirty-buffers`, `templates`, `font-moods`, `palette-types`, `schema-for`, `page-for`, the suggestion lists, and the LSP websocket) — takes an authenticated `Session`. The `/_schema/...` interception in the published-file handler is editorial too and is gated. The default is **gated**: a new `/_presemble` route is authenticated unless deliberately exempted.

**Published output stays public.** Serving the built site (the fallback file handler) requires no session — a published Presemble site is a public website (ADR-051: the boundary is the editorial plane, not readership). The **live-reload websocket (`/_presemble/ws`) is a deliberate public exception**: it carries only `reload` (page URL paths) and `resync`. Editorial events (structural anchors, cursor scroll, suggestion activity) travel on a separate session-gated socket; see the 2026-10 WebSocket amendment below. *(The original V0 text claimed this socket carried "never unpublished content". That was false: it also carried anchors with heading text from unsaved buffers, cursor-scroll anchors, and suggestion-list events naming draft files. Corrected by that amendment.)*

**V0 is authentication, not authorization.** Any valid session grants full editorial access to whatever site the `serve` process is serving. This is sufficient because one conductor serves one account's workspace (ADR-035/053, one conductor per account); per-principal / per-site authorization is deferred to V1+ and must not be faked here.

**The boundary is type-enforced, not a per-route check.** Authentication lives in the `accounts` component: a `Session` newtype with no public constructor from untrusted input, produced only by `SessionStore::verify` / `verify_str`, and an axum `FromRequestParts` extractor that returns `401` before any handler body runs. An editorial handler names `Session` in its signature; a handler that omits it cannot obtain one, and the editorial routes are additionally isolated on a nested router. "Make illegal states unrepresentable" applied to a security boundary: an ungated editorial handler does not compile. V0 ships an HMAC-SHA256 `DevSessionStore` with a `--dev-session` local-login flow; V1 swaps in a magic-link `SessionStore` implementing the same trait, with no handler changes.

## Amendment (2026-10): editorial modes and explained 401s

`presemble serve` runs in one of three **editorial modes**, chosen at startup:
- **Hosted** — `PRESEMBLE_SESSION_KEY` is set → `MagicLinkSessionStore` (ADR-055). `--dev-session` is ignored with a warning.
- **DevLogin** — `--dev-session <label>` → `DevSessionStore` with a random key; the login URL is printed.
- **Closed** — neither → `ClosedSessionStore`, which never verifies any token. **This is the default: editing is closed unless explicitly enabled.**

In Closed mode the published site and the public live-reload websocket work normally; every editorial route, including `/_presemble/editorial-ws`, as well as `/_schema/*` and `/_presemble/login`, answers 401.

**401s explain themselves.** The `Unauthorized` rejection has two variants: `NotLoggedIn` (the store can issue sessions but the request has none valid) and `EditingDisabled` (the store can never issue one). The variant is chosen by `SessionStore::login_availability()` (`Open` / `Closed`). The response is `401` with `www-authenticate: Cookie realm="presemble"` and a JSON body `{"ok":false,"error":"<message>"}`. The message is fixed text and never contains token, principal or key material. The extractor and the `/_schema` check share one function, `accounts::authenticate`.

Closed is a store type, not a flag, so "this server can't issue sessions" is reported by the boundary as a type rather than as a branch in each handler. The type boundary is unchanged: `ClosedSessionStore` has no key and no mint, and `Session` still has no public constructor.

## Amendment (2026-10): public/editorial WebSocket split

The browser event stream is split into two sockets with separate message types:

- **`/_presemble/ws` (public).** Carries `PublicMessage` only: `{"type":"reload","pages":[…]}` and `{"type":"resync"}`. No anchor, no `primary`, no file paths, no suggestion events. Pages served without a session receive a reader-only live-reload script that connects here.
- **`/_presemble/editorial-ws` (editorial).** Takes `Session` like every other editorial route, so it answers the standard JSON 401 (`NotLoggedIn` / `EditingDisabled`) before any upgrade. It also refuses browser upgrades whose `Origin` doesn't match `Host` (403), because the session cookie is `SameSite=Lax`, which doesn't stop same-site, cross-origin pages. It carries `EditorialMessage`: `reload` (with `primary` and a structural anchor), `scroll`, `suggestion-list-changed` and `resync`. Pages served to a valid session receive the editor UI, which connects only here. Each browser holds exactly one socket, so the editorial stream includes all public reloads.

**The split is type-enforced.** `PublicMessage` has no field that can hold an anchor, a file path or suggestion data. The only way onto the public channel is `EditorialMessage::to_public`, an exhaustive match with no wildcard, so a new editorial variant cannot reach the public socket without a reviewed decision. The two broadcast channels are typed separately, so an editorial message on the public channel does not compile. The same Origin check guards `/_presemble/lsp`.

**Session lifetime on the socket.** V0 session tokens have no expiry, no revocation and no logout. A session becomes invalid only when the key changes, which restarts the process and drops every socket. So the editorial socket authenticates once, at upgrade. If the browser's editorial socket can't reconnect and `GET /_presemble/session` answers 401, the page reloads and is served in its reader form.

**Known gap (accepted, not fixed by this amendment).** `serve` publishes its working tree, not a published tier. Unsaved editor buffers are rendered into the output directory and served by the public file handler. Page URLs for content that exists only in the working tree, and build-error pages, can be read anonymously. The public `pages` list therefore exposes nothing the file handler doesn't already serve, but `serve` does not meet "you should not see what has not been published to you" for page bodies. The real boundary is the published tier of ADR-048/049/052. Public reloads should eventually be driven by publish events from that tier rather than by working-tree rebuilds.

**Deferred:** re-validating the editorial socket while it is connected (needed once sessions gain expiry, revocation or logout); and whether the conductor's HTML should be readable anonymously at all in Hosted mode.

**Consequences:** cursor-follow scrolling and suggestion live-refresh now need a logged-in browser (`--dev-session` locally). The only public endpoints under `/_presemble/*` are `/_presemble/ws` and `/_presemble/login`.

## Amendment (2026-10): audiences are served different outputs (ADR-068)

ADR-068 closes the "known gap" of the WebSocket amendment above:

- **What each audience is served.** A request with an editor session is served the editorial output (the conductor's working copy, with editor attributes and build-error pages). Every other request is served the published output: built from the files on disk through the publish gate, without editor attributes. A page missing from it is a plain 404. `serve` without a session serves the published output; previewing drafts needs `--dev-session`.
- **Reload channels.** `/_presemble/ws` (public) announces changes to the published output only: it fires after the published output has been rebuilt (after a save or an external file change), never for an edit in progress. `/_presemble/editorial-ws` keeps the conductor's events.
- **Superseded detail.** The public channel is no longer fed by projecting editorial messages (`EditorialMessage::to_public` is gone). It has its own sender that accepts only `PublicMessage`, so an editorial message still cannot reach it; the conductor's rebuild events go to the editorial channel only.

## Why

1. **A single shared password could not survive a real link.** Before V0 the editorial plane was gated only by one shared Caddy `editor` password — world-writable to anyone who had it, with no per-user suggestion authorship. Accounts make an app-level session boundary mandatory.
2. **"You should not see what has not been published to you."** The core security principle (architect skill) means editorial *reads*, not just writes, must be gated — hence the whole-`/_presemble` rule rather than "gate POSTs only."
3. **Type-enforcement beats vigilance.** A `Session`-typed extractor makes the check un-forgettable on any handler that carries it; a runtime `if unauthorized` sprinkled per route is exactly what gets missed on the next added endpoint.
4. **Authn-first is honest for the alpha.** One conductor = one account's workspace, so authentication is the boundary that matters now; authorization is real work that should not be stubbed.

## Alternatives considered

- **Gate only mutations (POSTs), leave GET reads public** — rejected; editorial GETs expose unpublished/schema/dirty-buffer state, which violates the "not published to you" principle.
- **Keep the shared Caddy password** — rejected; not per-user, trivially shared, no authorship, unfit for invitees.
- **Runtime prefix-match middleware as the primary gate** — rejected as the load-bearing mechanism; a string-prefix `if` is the forgettable check the type boundary exists to replace. (A nested router is used as an audit aid, not the guarantee.)
- **Gate the live-reload websocket too** — rejected; it would break public live reload. Instead, editorial events moved to the gated `/_presemble/editorial-ws` (2026-10 amendment).
- **Do per-site authorization now** — deferred; unnecessary while one conductor serves one account, and real enough to deserve its own design (V1+).

## Consequences

- The `accounts` component owns authentication (`Session`/`SessionToken`/`Principal`, `SessionStore`, the extractor); it is the seam V1's magic-link store and `Account` types grow into.
- Every future `/_presemble` route must take `Session` (gated by default); exempting one is a deliberate, reviewable act. The public exceptions are exactly: published file serving and the live-reload ws.
- The Caddy `editor` basic_auth is now redundant for editorial endpoints and can be retired once V0 is verified in production (a separate devops follow-up; do not remove it in the same change that adds the app gate).
- **Known V0 gaps (accepted):** authn-only (no per-principal authorization); the live-reload ws is public (reload page paths and resync only; see the WebSocket amendment); the `--dev-session` flow prints a token usable via a URL query param (dev-only, localhost convenience).
- The default `serve` (no flags, no key) has editing closed; user docs must say to run `--dev-session` and open the printed Login URL to edit locally.
- Builds on ADR-031 (conductor as sole authority), ADR-035 (one conductor serves one account's workspace), ADR-051 (accounts / the editorial-plane-not-readership boundary), ADR-053 (this is its Task 1). Shipped in commit `c6dbf10`.
