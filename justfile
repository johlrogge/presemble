# presemble task runner.
# `just` is provided by devenv; run `just` with no args to list targets.

# Resolve the conductor's floating IP from the openstack module so the DNS
# module never has to hardcode it. This is the "lookup step": a plain
# `tofu output` you can read and run by hand if you ever need to.
_conductor-ip := `tofu -chdir=infra/openstack output -raw floating_ip 2>/dev/null || true`

# DNSimple credentials live outside the repo as plaintext (chmod 600), the same
# pattern as ~/.config/openstack/clouds.yaml. The dns-* recipes source this file
# just-in-time into the single tofu command — it is never exported into your
# shell or shell history. See infra/dns/README.md.
secrets-file := env_var('HOME') / ".config/presemble/secrets.env"

# List available targets.
default:
    @just --list

# One-time provider download for the DNS module.
dns-init:
    tofu -chdir=infra/dns init

# Preview the conductor DNS record against the live floating IP.
# Credentials are sourced from ~/.config/presemble/secrets.env just for the tofu call.
dns-plan:
    @test -f {{ secrets-file }} || { echo "missing {{ secrets-file }} — see infra/dns/README.md"; exit 1; }
    set -a && . {{ secrets-file }} && set +a && tofu -chdir=infra/dns plan -var floating_ip={{ _conductor-ip }}

# Apply the conductor DNS record, pointing it at the live floating IP.
# Credentials are sourced from ~/.config/presemble/secrets.env just for the tofu call.
# Non-interactive (-auto-approve) so it runs over the just MCP; `just dns-plan`
# is the review gate — run it first.
dns-apply:
    @test -f {{ secrets-file }} || { echo "missing {{ secrets-file }} — see infra/dns/README.md"; exit 1; }
    set -a && . {{ secrets-file }} && set +a && tofu -chdir=infra/dns apply -auto-approve -var floating_ip={{ _conductor-ip }}
