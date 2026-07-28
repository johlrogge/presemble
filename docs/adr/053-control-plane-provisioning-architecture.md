# ADR-053: Control-plane provisioning architecture for the hosted alpha

## Status
Proposed

## Decision

Hosted Presemble needs a **control plane**: the thing that turns "create an account, create a site" into a running, TLS-terminated, auth-gated editorial URL. The editorial happy path — create → preview → edit → suggest — is already built: the conductor is a multi-site workspace server (ADR-035, "site as a parameter"; ADR-031, sole authority), and the browser is already a conductor client. This ADR is scoped to a **September demo spike**, but it sets the seam for the real hosted alpha, so its abstractions must survive the transition.

**Two independent layers — isolation and density — and they must not be conflated:**

- **Isolation contract: one conductor per owner account, never shared across accounts.** Within an account's conductor, that account's sites live as workspace sites (ADR-035), so the per-account conductor is *cheap*: multi-site is a capability we already have. This **resolves the open question ADR-051 parked** — "process-per-account vs one shared multi-tenant conductor" — in favour of **per-account isolation at the conductor level**.
- **Density: several per-account conductors are packed onto one VM.** The cheaper tiers **never** get a dedicated VM — a dedicated VM per account is a **premium** upgrade. So multi-account-per-VM is the *default and permanent* hosting shape, not a demo shortcut. VMs are shared; conductors are not.

**A `ConductorProvisioner` abstraction is the core deliverable.** One interface, multiple implementations, **selected by profile/config** — the same move as the `mem_`/`fs_` `site_repository` split (ADR-028 profile polymorphism). The interface, not the implementation, is the commitment. The two implementations are **tiers**, not floor-versus-headline:

- **`SharedVmProvisioner`** — the **default / cheaper tiers**. Packs a per-account conductor (process/container) onto a shared VM alongside other accounts' conductors. Seconds to bring up, high density, per-account isolated. This is also exactly what the **September demo runs** (several accounts' conductors on the box we already operate), so the demo exercises the *real mainline model*, not a stub.
- **`DedicatedVmProvisioner`** — the **premium tier**. A whole VM per account on the Bahnhof OpenStack VPC via the existing OpenTofu IaC. Strongest isolation, at the cost of VM-boot latency and per-VM DNS/TLS/readiness. A special case layered on top of the default, not the north star for everyone.

**The control plane owns placement and scaling.** Because density is multi-account-per-VM, the control plane must **schedule each account's conductor onto a VM** (pick host + allocate a port), record that placement, route the account's subdomain to it, and **scale horizontally by adding VMs** as the fleet fills. Placement, fleet inventory, and subdomain→conductor routing are first-class control-plane responsibilities — this is the substance of "how to best utilize OpenStack": density and fleet management, not single-tenant boot time.

**New Polylith surface:**
- **base `control_plane`** — an axum service deployed to the existing box. It is a **conductor client in the ADR-031 sense** (it drives conductors, it is not one), plus the account/routing/placement frontend.
- **component `accounts`** — magic-link / email auth (ADR-051's leaning), with `Session` and `Account` **newtypes** that make "an editorial mutation without a `Session`" a *compile-time impossibility*, not a runtime check.
- **component `provisioning`** — the `ConductorProvisioner` trait, its `SharedVmProvisioner` / `DedicatedVmProvisioner` implementations, and the **placement/scheduling** logic (account → VM host + port).
- **component `site_registry`** — the owner → site → subdomain → status manifest, plus the account → VM placement map. This is the SaaS analog of ADR-050's per-tenant name→hash manifest, but at **site/placement granularity** rather than content-hash granularity: it is about *where a conductor runs and whether it is up*, not about deduped bytes.

**A `serve()` editorial-plane auth boundary is a hard dependency** (ADR-051 / M11 minimum). `/_presemble/*` editing endpoints must require an authenticated session; **published output stays public by design**. Today the editing plane is gated only by a single shared Caddy `editor` password — inadequate the moment a real invitee gets a link. This is **the first task**, and it is *independently valuable safety work* that lands value even if nothing else in this ADR ships on time.

**Routing and TLS.** Wildcard `*.presemble.pub` DNS + Caddy **on-demand TLS** + **dynamic per-subdomain routing** to the placed conductor (host + port), replacing today's single hardcoded vhost. The control plane owns the subdomain→conductor map via the `site_registry`.

**Explicitly out of scope / deferred:**
- **ADR-052 published-tier distribution** — for the demo, *preview is the conductor's own served output*. No Bahnhof-S3-seed → CDN pipeline is needed to demo the editorial loop.
- **ADR-051 seat pools, invite tree, quotas, billing** — the human-layer economics are settled on paper; none of them are on the demo critical path. (The tier split here — shared-VM default vs dedicated-VM premium — is where the *premium* lever will eventually attach.)
- **ADR-050 shared-CAS multi-tenancy plumbing** — per-account isolation here is conductor/process-shaped, not shared-hash-universe-shaped.
- **Blue-green reprovisioning** and **remote Helix / SSH jack-in** — the browser is already the conductor client; SSH editing is not needed for this alpha.

## Why

1. **The editorial product already exists.** ADR-031 and ADR-035 mean create/preview/edit/suggest is *reuse*, not new work. The control plane is the missing ring around a working core: identity, provisioning, placement, routing.
2. **Isolation and density are different questions.** One conductor per account (never shared) is the isolation *contract*; how many conductors share a VM is a *density* decision. Separating them lets cheap tiers pack many accounts per VM while premium buys a dedicated VM — without ever weakening the per-account isolation contract.
3. **Multi-account-per-VM is the permanent cheap-tier shape.** Dedicated VMs don't fit the economics of a free/cheap tier, so density is not a demo compromise — it is the mainline. The demo therefore runs the real model.
4. **The provisioner interface makes the tier choice a wiring detail.** ADR-028's profile polymorphism lets the same call sites target shared-VM or dedicated-VM provisioning; the tier is config, not architecture.
5. **The auth boundary is safety work we owe regardless.** A single shared editor password cannot survive a real invitee link. Gating `/_presemble/*` behind a session is required by ADR-051's M11 and is worth doing first on its own merits.

## Alternatives considered

- **One shared multi-tenant conductor across accounts** — the other horn of ADR-051's open question. Rejected: it puts all accounts in one blast radius and weakens the isolation *contract*. Density is achieved instead by packing *separate* per-account conductors onto a shared VM, which keeps isolation while still sharing infrastructure.
- **VM-per-account as the default** — rejected as the economic model: a dedicated VM per account cannot underwrite a free/cheap tier. It is retained as the **premium** tier only.
- **Skip the abstraction; hardcode the shared-VM path** — rejected. The premium (dedicated-VM) tier is a stated product direction, so the interface earns its keep immediately; it is not speculative.
- **Remote Helix / SSH jack-in for editing** — rejected as unnecessary; the browser is already the conductor client (ADR-031).

**OpenStack utilisation approaches** (what the provisioners run underneath, and what devops is prototyping — reframed toward density and fleet, not per-account boot time):
1. **Many per-account conductors on one VM** (containers or processes, Caddy routing subdomain→port) — the `SharedVmProvisioner` default. **devops prototypes this first**: process/container orchestration, port allocation, lifecycle, and routing on a single VM.
2. **Horizontal fleet scaling** — add VMs (via the existing OpenTofu IaC) and place new accounts across them; a placement/scheduler concern in the control plane. **devops prototypes this second**: how a new VM joins the fleet and starts accepting placements.
3. **Dedicated VM per account** — the `DedicatedVmProvisioner` premium path; Tofu-per-tenant apply with a readiness gate. Layered on top once the default is solid.
4. **Nova / Heat native orchestration / pre-warmed pools** — research-only optimisations for latency and elasticity, not on the demo path.

## Consequences

- **New Polylith surface** to build and wire: base `control_plane`; components `accounts`, `provisioning` (incl. placement/scheduling), `site_registry` (incl. account→VM placement map). `provisioning` follows the ADR-028 profile-selected implementation pattern.
- **`serve()` grows a published/unpublished auth boundary** (ADR-051 / M11) — the single shared Caddy `editor` password is retired for editorial endpoints; published output stays public. First task; independently shippable.
- **Placement becomes a real subsystem** — the control plane tracks a fleet of VMs, packs per-account conductors onto them (host + port), and scales by adding VMs. Port allocation, host selection, and a fleet inventory are new concerns.
- **Caddy config becomes dynamic** — wildcard `*.presemble.pub`, on-demand TLS, per-subdomain routing to a placed conductor (host + port), replacing the hardcoded vhost.
- **devops carries a density/fleet deliverable** — prototype many-conductors-per-VM orchestration and horizontal VM scaling, with dedicated-VM-per-account as the premium special case. (Not a per-account-VM latency race.)
- **Readiness gating** — a conductor (shared-VM process or dedicated VM) must expose a "up and answering" gate before a subdomain is routed to it; a routed-but-not-ready conductor is a broken URL.
- **The premium lever has a home.** The shared-VM/dedicated-VM tier split is where ADR-051's premium/paid isolation upgrade will eventually attach.
- Builds on **ADR-035** (multi-site conductor — why per-account is cheap), **ADR-031** (control plane as conductor client), **ADR-051** (accounts / auth boundary it depends on; **resolves its process-per-account-vs-shared open question**), **ADR-028** (profile polymorphism — the provisioner pattern). **Defers ADR-052** (published-tier distribution) and **ADR-050** (shared-CAS tenancy); **relates to ADR-050** as a coarser, site/placement-granularity registry.

## Open questions

- **Density limits** — how many per-account conductors fit on one VM before contention (memory/CPU per idle vs active conductor), and what triggers adding a VM. Decided by devops's density prototype.
- **Placement policy** — bin-pack for density vs spread for blast-radius; how premium (dedicated-VM) accounts are carved out of or excluded from the shared fleet.
- **Isolation mechanism on a shared VM** — process vs container per account, and what that buys for security and resource limits.
- **Readiness-gate mechanism** — how the control plane confirms a fresh conductor is live before routing a subdomain to it (health command vs connection probe vs Tofu output signal).
- **Site-registry ↔ tenancy convergence** — how the site/placement-granularity `site_registry` here relates to (or later folds into) ADR-050's content-hash manifest once shared-CAS tenancy lands.
