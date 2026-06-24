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

# Stopgap live-edit basic_auth: the Caddy bcrypt hash lives outside the repo
# (CADDY_EDITOR_HASH, single-quoted so its `$` survives sourcing). The conductor
# tofu recipes source it just-in-time into -var. Generate it with: just conductor-auth.
auth-file := env_var('HOME') / ".config/presemble/presemble-auth.env"

# ssh-mcp config (Host `conductor`) — reused by deploy so scp/ssh use the
# dedicated presemble key and the project-local known_hosts.
ssh-config := env_var('HOME') / ".metadev/projects/presemble/.ssh/config"

# List available targets.
default:
    @just --list

# --- conductor (openstack module) ---

# Preview changes to the conductor box / network. The Caddy basic_auth hash is
# sourced from the auth-file just-in-time into -var (rendered into cloud-init).
conductor-plan:
    @test -f {{ auth-file }} || { echo "missing {{ auth-file }} — run: just conductor-auth"; exit 1; }
    set -a && . {{ auth-file }} && set +a && tofu -chdir=infra/openstack plan -var caddy_editor_hash="$CADDY_EDITOR_HASH"

# Apply the conductor box / network (non-interactive; plan first).
conductor-apply:
    @test -f {{ auth-file }} || { echo "missing {{ auth-file }} — run: just conductor-auth"; exit 1; }
    set -a && . {{ auth-file }} && set +a && tofu -chdir=infra/openstack apply -auto-approve -var caddy_editor_hash="$CADDY_EDITOR_HASH"

# Re-provision the conductor box from scratch: destroys the instance and creates
# a fresh one from cloud-init (this is how new cloud-init — systemd unit, Caddy
# auth — gets baked, since user_data has ignore_changes). The floating IP
# re-associates so the public address is unchanged. (Brief downtime — the
# blue-green workstream will make this zero-downtime later.)
conductor-replace:
    @test -f {{ auth-file }} || { echo "missing {{ auth-file }} — run: just conductor-auth"; exit 1; }
    set -a && . {{ auth-file }} && set +a && tofu -chdir=infra/openstack apply -auto-approve -var caddy_editor_hash="$CADDY_EDITOR_HASH" -replace=openstack_compute_instance_v2.conductor

# Cross-compile the conductor binary for the Bahnhof box (Debian 13 trixie,
# x86_64, glibc 2.41). cargo-zigbuild pins the glibc version with a standard ELF
# interpreter so the binary runs on stock Debian; profile `live` selects
# fs_site_repository (production). Output: target/x86_64-unknown-linux-gnu/release/content_management
conductor-build:
    ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig" cargo polylith cargo --profile live zigbuild --release --target x86_64-unknown-linux-gnu.2.41 --bin content_management

# Cross-compile the `presemble` binary for the box — THIS is the deploy artifact
# (the live-edit server: `presemble serve <site-dir>`). Same zigbuild pipeline as
# conductor-build, just --bin presemble. Output: target/x86_64-unknown-linux-gnu/release/presemble
presemble-build:
    ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig" cargo polylith cargo --profile live zigbuild --release --target x86_64-unknown-linux-gnu.2.41 --bin presemble

# One-time: generate the stopgap live-edit credential (user `editor`). Makes a
# random password, hashes it with the box's caddy, and writes the hash (single-
# quoted) + the plaintext to the auth-file (chmod 600, outside the repo). Prints
# the password once. Real per-user control is M11.
conductor-auth:
    #!/usr/bin/env bash
    set -euo pipefail
    pw="$(openssl rand -base64 18)"
    hash="$(ssh -F {{ ssh-config }} conductor "caddy hash-password --plaintext '$pw'")"
    mkdir -p "$(dirname {{ auth-file }})"
    ( umask 177; printf "PRESEMBLE_EDITOR_USER=editor\nPRESEMBLE_EDITOR_PASSWORD='%s'\nCADDY_EDITOR_HASH='%s'\n" "$pw" "$hash" > {{ auth-file }} )
    echo "Wrote {{ auth-file }} (user: editor)"
    echo "  password (store this): $pw"

# Deploy the built presemble binary + a site to the box and (re)start the
# service. THIS is what CI runs on master. Defaults to the cross-compiled binary
# and the repo's site/. The box must already be provisioned (cloud-init).
conductor-deploy bin="target/x86_64-unknown-linux-gnu/release/presemble" site_dir="site":
    test -x {{ bin }} || { echo "missing {{ bin }} — run: just presemble-build"; exit 1; }
    scp -F {{ ssh-config }} {{ bin }} conductor:/tmp/presemble.new
    tar -C {{ site_dir }} --exclude=./output --exclude=./.presemble --exclude=./.devenv -czf - . | ssh -F {{ ssh-config }} conductor 'cat > /tmp/site.tgz'
    ssh -F {{ ssh-config }} conductor 'sudo install -o root -g root -m0755 /tmp/presemble.new /usr/local/bin/presemble && sudo mkdir -p /var/lib/presemble/site && sudo tar -C /var/lib/presemble/site -xzf /tmp/site.tgz && sudo chown -R presemble:presemble /var/lib/presemble && sudo systemctl restart presemble && rm -f /tmp/presemble.new /tmp/site.tgz && sleep 1 && systemctl is-active presemble'

# --- DNS (dnsimple module) ---

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
