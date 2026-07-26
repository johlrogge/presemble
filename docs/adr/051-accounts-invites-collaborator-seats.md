# ADR-051: Accounts, invite bootstrap, and collaborator seats

## Status
Proposed

## Decision

Hosted Presemble adds a **human layer** on top of the tenancy substrate of ADR-050: who a person is, what they own, and how they get in. This is the concrete shape of the M11 per-user auth milestone. The rollout is an **invite-only alpha/beta** in the spirit of Gmail's and Spotify's launches — scarcity as controlled load-ramp and as accountability, not just marketing.

**An account is a person who logs in.** There are **two account types**, both real logins, each minted by an invite:
- **Owner accounts** — create and own N sites; pay for seats and storage quota.
- **Collaborator-only accounts** — free; **cannot create their own sites**; hold editorial access to sites they are invited to.

An account maps onto the existing tenancy model: an owner account is a **tenant** (ADR-050), i.e. a namespace of refs, and owns one or more **sites** within its **workspace** (ADR-035, "site as a parameter"). No org/team layer yet.

**An invite is the right to create an account.** The invite *type* determines which account it mints: a founder- or tier-invite mints an **owner**; an owner's collaborator invite mints a **collaborator-only** account. Growth is an **invite tree** — every account traces to an inviter.

**Invite tiers ramp formality:**
- **Tier 1** (founder → friends/colleagues) — informal, personal, feedback-oriented ("try my thing, tell me what you think"). Trusted; generous limits.
- **Tier 2** (invitees who earn a few invites of their own) — formal: real quotas and a **bounded invite budget** per account. Gating tightens as the tree widens.

**Collaborator seats — collaboration is the product wedge, so a free floor is deliberate.**
- **Seats are per-account**, not per-site: an owner holds a **pool** of collaborator seats and assigns collaborators to their sites.
- **Collaboration is always free for the collaborator.** The **site owner pays** per seat *beyond the free floor*.
- Every account gets **1–2 always-free collaborator seats** (exact number a tunable). This is a strategic floor — never zero — because in-browser editorial collaboration (comment / suggest / track-changes) is Presemble's differentiator versus the Netlify/Vercel Jamstack-host category, which host *output* but not the collaborative content loop.
- **Collaborator-only accounts are a conversion funnel** → future site-creating owners.

**Two independent throttle levers:**
- **Entry** is gated by the **invite tree** (who gets an account at all).
- **Consumption** is gated by a **storage-GB quota** — because Bahnhof S3 egress is free (ADR-050), *stored bytes* are the scarce resource, not bandwidth.

**Monetization levers** (the alpha is free; these define where money later enters):
- **Storage-GB quota** upgrades.
- **Paid collaborator seats** beyond the free 1–2.
- **Granular per-site access control** as a candidate premium feature (fine-grained "which collaborator reaches which site").
Seats + collaboration are the clearest combined **revenue and funnel** story; metered CDN egress (ADR-052) is the usage-fair billing complement. A concrete **billing/metering mechanism is not yet specified** (see open question).

**Auth boundary is the editorial plane, not readership (M11).** To **edit, comment, suggest, or collaborate** you must log in. To **view a generated site** you simply surf to it — the published static output is **public by design** (a site owner opting into gated readership is a later, separate feature). Concretely, M11 = per-user auth **plus** enforcing a published/unpublished boundary in `serve()`, which today has none. Auth mechanism leans **magic-link / email**, which fits the "here is a link, try it" alpha informality.

**The alpha exists to learn.** Two explicit learning goals drive it:
1. **Shared-graph vs isolation** — instrument cross-account content-hash reuse (ADR-050's shared CAS is the *instrument*; per-account isolation is the user-facing *contract*) to decide empirically whether Presemble should be "one huge graph like GitHub" or isolated per account. Measure-first, stay reversible.
2. **Preference and competitive position** — what users *like*, and how they rate Presemble against the **Netlify/Vercel** category, testing whether collaboration is the felt differentiator.

## Why

1. **The substrate already exists; this is the human layer.** ADR-050 settled tenancy (a tenant = a namespace of refs, the conductor is the trust boundary). Accounts, invites, and seats are the identity and access story on top — kept deliberately separate from the CAS plumbing.
2. **Invite-only is load-ramp *and* accountability.** It fills the pre-paid Bahnhof terabytes with real, learnable usage at a controlled rate, and the invite tree makes abuse a prunable subtree rather than whack-a-mole.
3. **A free collaborator floor is strategic, not charity.** Collaboration is the wedge against Netlify/Vercel; charging for *any* collaboration would blunt the one thing that differentiates Presemble.
4. **Editorial-plane auth keeps the open web open.** Published sites stay public; only authoring is gated — Presemble hosts websites, not walled gardens.
5. **Two account types create a funnel.** Free collaborators experience Presemble at no cost and convert into owners.

## Alternatives considered

- **Open (non-invite) signup for the alpha** — rejected; loses controlled load-ramp, accountability, and the pre-paid-capacity learning frame.
- **Org/team as the primary unit** — deferred; the account-as-person model is simpler and sufficient for alpha. Teams can layer on later.
- **Per-site seats** — rejected in favour of a per-account seat pool the owner allocates; simpler to reason about and to bill, with granular per-site control as a premium upgrade rather than the default.
- **Zero free collaborators (seats fully paid)** — rejected; would undercut the collaboration wedge.
- **Time-limited free seats** (an earlier idea) — dropped in favour of a small *permanent* free floor; a trial-that-expires reads as hostile for the very feature meant to hook users.
- **Gate published-site readership behind login during alpha** — rejected as the default; published output is a public website. Opt-in gated readership is a possible later feature.

## Consequences

- A new **authenticated account/identity subsystem** (M11): account records, two account types, session/auth (leaning magic-link/email), and an **invite-tree** store (inviter → invitee edges, per-tier invite budgets).
- A **seat model**: per-account seat pool, owner-pays-beyond-free-floor, collaborator-always-free, assignment of collaborators to sites; granular per-site access flagged as premium.
- **`serve()` gains a published/unpublished boundary** — the editorial plane requires auth; published output is public. (The architect flagged that no such boundary exists today; this is now on the critical path for hosting.)
- **Quota enforcement** on storage-GB per account, tighter at tier 2.
- **Instrumentation for cross-account hash reuse** must exist from the start so the shared-graph-vs-isolation question is answered by data, not guesswork.
- **Billing/metering is implied but unspecified** — paid seats, quota upgrades, and metered CDN egress (ADR-052) all assume a payment mechanism that a later ADR must define.
- Builds on ADR-050 (multi-tenancy / shared hash universe), ADR-035 (workspace conductor), ADR-048 (content lifecycle), ADR-031 (conductor as sole authority), ADR-019 (suggestion nodes — the collaboration primitive). Distribution of the public output is ADR-052.

## Open questions

- **Exact free-seat number** — 1 vs 2 per account.
- **Granular per-site access as premium** — confirmed as a candidate lever, not yet committed as paid.
- **Billing/metering mechanism** — undefined; needed before money enters (paid seats, quota, metered egress).
- **Multi-tenant conductor shape** — process-per-account vs one shared multi-tenant conductor (ADR-031 is sole-authority-per-workspace today). Alpha scale may make process-per-account acceptable, with the shared model as a later, data-informed optimization.
