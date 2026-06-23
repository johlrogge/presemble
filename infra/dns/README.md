# presemble DNS (DNSimple)

OpenTofu config for presemble's DNS records, hosted on [DNSimple]. It manages
**records only** — not the zone — so its blast radius is exactly what it
provisions (see the note in `main.tf`).

Currently it publishes one record:

    conductor.presemble.pub  A  <conductor floating IP>

## Prerequisites

1. **The domain is registered and delegated to DNSimple.** Register
   `presemble.pub` on DNSimple (or register elsewhere and point the nameservers
   at DNSimple). The zone must exist in your DNSimple account before applying.
2. **The conductor exists** in the `infra/openstack` module (it owns the
   floating IP this record points at).
3. **Credentials in the environment** — never in the repo. The provider reads:

       export DNSIMPLE_ACCOUNT=<account-id>
       export DNSIMPLE_TOKEN=<account-api-token>

   Create the token under DNSimple → Account → Automation → API tokens. It is an
   *account* token (DNSimple has no per-domain tokens), so treat it like the
   OpenStack password: shell env only, never committed. It is deliberately not a
   Tofu variable, so it can't leak into state or a plan log.

## Usage

The floating IP is not hardcoded — a `just` target looks it up from the
`infra/openstack` module and passes it in:

    just dns-init     # one-time: download the provider
    just dns-plan     # preview
    just dns-apply    # publish the record

`dns-plan`/`dns-apply` run, under the hood:

    tofu -chdir=infra/dns <cmd> -var floating_ip=$(tofu -chdir=infra/openstack output -raw floating_ip)

So `infra/openstack` must have been applied first (its state holds the IP).

## Sandbox

DNSimple runs a free [sandbox] for dry runs. Point a throwaway zone there and:

    export DNSIMPLE_SANDBOX=true        # or: -var dnsimple_sandbox=true
    just dns-plan

## Safety notes

- **Records only, never the zone** — see `main.tf`.
- **`prevent_destroy`** on the conductor record blocks an accidental
  `tofu destroy` from dropping the conductor's public name. Remove it
  deliberately if you ever need to tear the record down.
- **TTL is low (300s)** while bringing things up so changes propagate fast;
  raise `record_ttl` once the address is stable.
- **State holds no secrets here** (the token stays in the env), but the state
  file is gitignored alongside the rest of the Tofu state regardless.

[DNSimple]: https://dnsimple.com/
[sandbox]: https://developer.dnsimple.com/sandbox/
