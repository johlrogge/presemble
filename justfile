# presemble task runner.
# `just` is provided by devenv; run `just` with no args to list targets.

# Resolve the conductor's floating IP from the openstack module so the DNS
# module never has to hardcode it. This is the "lookup step": a plain
# `tofu output` you can read and run by hand if you ever need to.
_conductor-ip := `tofu -chdir=infra/openstack output -raw floating_ip 2>/dev/null || true`

# List available targets.
default:
    @just --list

# One-time provider download for the DNS module.
dns-init:
    tofu -chdir=infra/dns init

# Preview the conductor DNS record against the live floating IP.
# Requires DNSIMPLE_ACCOUNT and DNSIMPLE_TOKEN in the environment.
dns-plan:
    tofu -chdir=infra/dns plan -var floating_ip={{ _conductor-ip }}

# Apply the conductor DNS record, pointing it at the live floating IP.
# Requires DNSIMPLE_ACCOUNT and DNSIMPLE_TOKEN in the environment.
dns-apply:
    tofu -chdir=infra/dns apply -var floating_ip={{ _conductor-ip }}
