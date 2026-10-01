# ADR-055: Auth hand-off: shared-key session tokens across control plane and conductors

## Status
Accepted

## Decision

Hosted Presemble runs a **control plane** (owner login, dashboard, provisioning — ADR-053) and, per account, a separate **`presemble serve` conductor** whose editorial plane is gated by the V0 auth boundary (ADR-054). This ADR defines how a logged-in owner on the dashboard gets into their per-account conductor's editorial UI — the **auth hand-off** — without the conductor calling back to any auth service in the request path.

**The cross-process trust root is a shared HMAC key.** A single secret, `PRESEMBLE_SESSION_KEY`, is held by the control plane and **injected into every per-account `presemble serve`**. Session tokens are HMAC-signed with that key; verification is **pure, stateless HMAC** over the token — no database lookup, no callback. Because the key is shared, a token minted by the control plane verifies inside any conductor started with the same key. This keeps the conductor the **sole authority** for its own requests (ADR-031): it validates the token itself, offline, with no network dependency in the hot path.

**Two session-store types, distinguished by payload prefix.**
- **`DevSessionStore`** — per-process *random* key, `dev:<label>` payloads. Local-dev only (the `--dev-session` flow). Cannot participate in hand-off (its key is not shared).
- **`MagicLinkSessionStore`** — the shared `PRESEMBLE_SESSION_KEY`, stateless HMAC, `acct:<account-id>` payloads. Used by the control plane (mint) and by every hosted conductor (verify).

`presemble serve` selects the store from the environment and flags (see the ADR-054 amendment):
- `PRESEMBLE_SESSION_KEY` present → `MagicLinkSessionStore` (`--dev-session` is ignored with a warning);
- absent, with `--dev-session <label>` → `DevSessionStore::new_random()`;
- absent, without the flag → `ClosedSessionStore` (editing disabled; no session can ever be obtained).

The **payload prefix is the discriminator**: a `dev:` token can't become an account session even under a shared key, because verification strips and matches the `acct:` prefix. A set-but-invalid key is a **fail-fast startup error** (see the 2026-10-01 amendment below).

**The hand-off flow (two token kinds, two places):**
1. **Login token** (`LoginTokenStore`) — short-TTL, single-use, HMAC-signed. Issued by the control plane, delivered via the emailed magic link. Its issue/verify/consume lifecycle lives **entirely in the control plane**.
2. The control plane verifies the login token (single-use, TTL) and establishes the owner's dashboard session.
3. **"Open editor"** → the control plane mints a **session token** (`acct:<id>`, shared key) and redirects the browser to `https://<subdomain>/_presemble/login?token=<session-token>`.
4. The conductor's **public `/_presemble/login`** route verifies that token by HMAC (`verify_str`), sets a **host-local `presemble_session` cookie**, and 302s into the editorial UI. The cookie — not the URL token — is the durable credential.

So the **login-token → session-token exchange happens at the control plane**; **session verification happens at the conductor**. The session token *is* what rides in the hand-off URL; it is short-lived by policy and the resulting cookie carries it thereafter.

**Cookies are host-local; the wildcard-domain cookie is rejected.** Each conductor sets its cookie for its own subdomain only. Setting the cookie for `.presemble.pub` would send one owner's editorial cookie to **every other tenant's subdomain** — a cross-tenant breach of "you should not see what has not been published to you." Rejected outright.

**Router topology is the auth invariant.** Gating is **per-handler**, via the `Session` `FromRequestParts` extractor appearing in a handler's signature — *not* per-router-layer. `/_presemble/login`, `/_presemble/ws`, and the published `file_handler` are public precisely because their handlers do not take `Session`; every editorial handler does. Mounting the public routes on the outer router chain therefore cannot ungate the editorial ones. **Any future refactor to nested-router or middleware-based gating must preserve this per-handler property**, or the boundary silently breaks.

**Crate-boundary invariant makes the public login route safe.** `SessionToken::from_raw` is crate-private; an untrusted string can only become a verified `Session` through `SessionStore::verify_str`. The public `/_presemble/login` handler thus cannot be turned into a bypass — a bad token yields 401 with no cookie; only an HMAC-valid token sets one, to the verified value verbatim. When the store is `ClosedSessionStore`, `/_presemble/login` answers 401 with the "Editing is disabled" explanation before attempting verification, and never sets a cookie.

**Cookie attributes:** `HttpOnly; SameSite=Lax; Path=/`; **`Secure` is omitted** because Caddy terminates TLS in front of the conductor. This is a deployment-coupled choice — if a conductor is ever exposed without a TLS-terminating proxy, `Secure` must be added.

## Why

1. **No auth service in the conductor's hot path.** Stateless shared-key HMAC lets the conductor validate requests itself (ADR-031), with no per-request callback, DB lookup, or network dependency — fast and failure-isolated.
2. **Simplicity via a shared secret.** One key, injected like the existing `caddy_editor_hash`, is far less machinery than a token-introspection endpoint or a shared session database queried on every request.
3. **Type-split keeps dev and prod honest.** Separate store types (not a boolean flag) make "random per-process key" and "shared key" different things at compile time; the payload prefix prevents accidental cross-use.
4. **Host-local cookies preserve tenant isolation** — the wildcard cookie would leak across tenants.
5. **Per-handler gating is robust to refactors of routing** and is already proven by the V0 tests.

## Alternatives considered

- **Wildcard `.presemble.pub` session cookie** (share one cookie across all subdomains) — rejected; leaks each owner's editorial cookie to every other tenant's subdomain.
- **Conductor calls the control plane to introspect each token** (OAuth-style) — rejected; puts a live auth-service dependency in the conductor's request hot path, contradicting ADR-031 (sole authority, offline validation) and adding latency + a failure mode.
- **Shared session database the conductor queries per request** — rejected for the same hot-path/network-dependency reason; stateless HMAC needs no lookup.
- **Independent per-conductor credentials (no SSO)** — rejected; the owner would re-authenticate at every site, defeating the dashboard's single sign-on.

## Consequences

- The control plane holds `PRESEMBLE_SESSION_KEY` and **injects it into every per-account `serve`** (box-local secret file → systemd `Environment=`, mirroring the `caddy_editor_hash` injection pattern; **never in the repo**).
- **Blast radius of the shared key = all conductors.** Mitigated by: one-conductor-per-account isolation (a leaked token only reaches the single account's content that conductor serves — ADR-053), short-TTL hand-off tokens, and the host-local cookie. Key rotation invalidates all live sessions (acceptable for the alpha).
- **The control plane must stay single-instance** — or move the login-token single-use `consumed` set to a shared store (Redis/DB). The in-memory single-use set is only sound in one process. ADR-053 scales *conductors/VMs*, not the control plane, so this holds for the alpha, but **must not be forgotten** if the control plane is ever horizontally scaled (it would silently reopen the login-token replay window).
- **Short-TTL session token rides in a URL** (`/_presemble/login?token=`) — it can land in logs / browser history / shoulder-surf. Hence: short TTL, and the durable credential is the resulting `HttpOnly` cookie, not the URL.
- The conductor's `/_presemble/login` is a **permanent public exception** alongside the public live-reload `/_presemble/ws` (the editorial socket `/_presemble/editorial-ws` is gated) (ADR-054); the per-handler `Session` gating invariant must be maintained.
- **`Secure` cookie flag** is omitted (TLS at Caddy); revisit if deployment changes.
- Builds on ADR-054 (V0 editorial-plane boundary this extends), ADR-051 (accounts / magic-link), ADR-053 (the control plane this is the auth contract for), ADR-031 (conductor as sole authority — why no external auth in the hot path). Implemented in commits `b94b4eb` (accounts: magic-link + login-token stores) and `7db1564` (serve: public `/_presemble/login` + env-selected store); the control-plane *minting* side is built in V1 Task 3.1.

## Amendment (2026-10-01): key format, validation, and selection

**Format.** `PRESEMBLE_SESSION_KEY` is an opaque UTF-8 string whose raw bytes are the HMAC-SHA256 key (no base64/hex decoding). Generate it with `openssl rand -base64 32` (44 characters, 256 bits of entropy).

**Validation.** The key is parsed once at process start into `accounts::SessionKey`, which cannot hold an empty, whitespace-only, or shorter-than-32-byte value. Every HMAC store (`MagicLinkSessionStore`, `LoginTokenStore`, `DevSessionStore`) accepts only a `SessionKey`. A set-but-invalid key is a fatal startup error in both `presemble serve` and the control plane, regardless of other flags. The error names the variable and the rule and never prints the key. Length is not entropy: the minimum blocks empty and trivially short keys, and operators must still generate keys randomly.

**Selection in `presemble serve`.** A valid key → Hosted (`MagicLinkSessionStore`). An invalid key → refuse to start. No key plus `--dev-session` → DevLogin. No key and no flag → Closed (`ClosedSessionStore`, editing disabled).

**Dev key.** `DevSessionStore::new_random()` draws 32 bytes from a CSPRNG (`rand` `ThreadRng`) and never reads the environment. Dev tokens never survive a restart and never verify in another process.

**Why.** `PRESEMBLE_SESSION_KEY=` (set but empty) previously selected Hosted with an empty HMAC key, letting anyone forge `acct:` sessions. The control plane accepted any non-empty key. The dev key was a 64-bit `DefaultHasher` digest of pid, time and a stack address.
