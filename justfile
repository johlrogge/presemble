# presemble task runner.
# `just` is provided by devenv; run `just` with no args to list targets.

# Resolve the conductor's floating IP from the openstack module so the DNS
# module never has to hardcode it. This is the "lookup step": a plain
# `tofu output` you can read and run by hand if you ever need to.
_conductor-ip := `tofu -chdir=infra/openstack output -raw floating_ip 2>/dev/null || true`

# Resolve the alpha box's floating IP from the alpha module (parallel state).
# Empty string when the alpha box has not been provisioned yet.
_alpha-ip := `tofu -chdir=infra/alpha output -raw floating_ip 2>/dev/null || true`

# DNSimple credentials live outside the repo as plaintext (chmod 600), the same
# pattern as ~/.config/openstack/clouds.yaml. The dns-* recipes source this file
# just-in-time into the single tofu command — it is never exported into your
# shell or shell history. See infra/dns/README.md.
secrets-file := env_var('HOME') / ".config/presemble/secrets.env"

# Stopgap live-edit basic_auth: the Caddy bcrypt hash lives outside the repo
# (CADDY_EDITOR_HASH, single-quoted so its `$` survives sourcing). The conductor
# tofu recipes source it just-in-time into -var. Generate it with: just conductor-auth.
auth-file := env_var('HOME') / ".config/presemble/presemble-auth.env"

# Alpha box session key — sourced just-in-time by the alpha-plan/apply recipes.
# Generate with: openssl rand -base64 32 and store as PRESEMBLE_SESSION_KEY.
alpha-secrets-file := env_var('HOME') / ".config/presemble/alpha-secrets.env"

# ssh-mcp config (Host `conductor`) — reused by deploy so scp/ssh use the
# dedicated presemble key and the project-local known_hosts.
ssh-config := env_var('HOME') / ".metadev/projects/presemble/.ssh/config"

# List available targets.
default:
    @just --list

# --- local development ---

# Output lands in <parent-of-dir>/output/<dir-name>/. The site is validated
# first (publish gate, ADR-061): on validation errors every diagnostic is
# printed on stderr as `path:line:col: error: …`, nothing is written, and the
# build exits non-zero. On render failure the CLI prints the count
# ("N pages built, M failed") followed by one "error:" line per page message.
# Build a site with the dev profile; exits non-zero on validation errors or failed pages.
site-build dir="site":
    #!/usr/bin/env bash
    set -uo pipefail
    log="$(mktemp)"
    trap 'rm -f "$log"' EXIT
    cargo polylith cargo --profile dev run --bin presemble -- build {{ dir }} 2>&1 | tee "$log"
    status=${PIPESTATUS[0]}
    if summary="$(grep -E '[1-9][0-9]* failed' "$log")"; then
        echo "site-build: {{ dir }} has failed pages:${summary}" >&2
        exit 1
    fi
    exit "$status"

# No build output is written into the repo; the temp path is printed and the
# output lands under <tmp>/output/<name>/.
# Build example-sites/<name> in a fresh temp dir.
site-build-example name="blog":
    #!/usr/bin/env bash
    set -euo pipefail
    test -d example-sites/{{ name }} || { echo "missing example-sites/{{ name }}"; exit 1; }
    tmp="$(mktemp -d -t presemble-example-{{ name }}.XXXXXX)"
    site="$tmp/{{ name }}"
    cp -r example-sites/{{ name }} "$site"
    # Same as SiteTemplate::scaffold (hiccup): example sites keep templates in templates/hiccup/.
    if [ -d "$site/templates/hiccup" ]; then
      cp -r "$site/templates/hiccup/." "$site/templates/"
      rm -rf "$site/templates/hiccup"
    fi
    rm -rf "$site/templates/html"
    echo "Temp site: $site" >&2
    {{ just_executable() }} --justfile {{ justfile() }} site-build "$site"
    echo "Temp output: $tmp/output/{{ name }}"

# Copies the build output of <dir> (built with site-build) into a fresh temp
# dir and prints its path. Use it to keep a "before" output: `presemble build`
# always writes to <parent-of-dir>/output/<dir-name>/, which the next build
# overwrites.
# Build a site and snapshot its output for a later output-diff.
site-snapshot dir="site":
    #!/usr/bin/env bash
    set -euo pipefail
    {{ just_executable() }} --justfile {{ justfile() }} site-build {{ dir }}
    out="$(dirname "$(realpath {{ dir }})")/output/$(basename "$(realpath {{ dir }})")"
    test -d "$out" || { echo "missing build output $out" >&2; exit 1; }
    snap="$(mktemp -d -t presemble-snapshot-$(basename "$(realpath {{ dir }})").XXXXXX)"
    cp -r "$out/." "$snap/"
    echo "Snapshot: $snap"

# Builds site/ as it is at git <ref> (a commit, tag or branch) in a throwaway
# worktree and snapshots the output, like site-snapshot does for the working
# tree. The worktree is removed afterwards; the snapshot is kept and its path
# printed, ready for output-diff (for example `just snapshot-ref HEAD` against
# `just site-snapshot` to prove a change leaves published output untouched).
# Uses its own target dir inside the worktree (a full build): sharing the main
# one lets the ref's binary overwrite target/debug/presemble, which cargo then
# mistakes for the working tree's up-to-date binary.
# Snapshot the published output of site/ at a git ref.
snapshot-ref ref:
    #!/usr/bin/env bash
    set -euo pipefail
    wt="$(mktemp -d -t presemble-ref.XXXXXX)"
    cleanup() { git worktree remove --force "$wt" >/dev/null 2>&1 || rm -rf "$wt"; git worktree prune; }
    trap cleanup EXIT
    git worktree add --detach "$wt" "{{ ref }}" >&2
    out="$(cd "$wt" && {{ just_executable() }} site-snapshot site)"
    echo "$out" >&2
    echo "$out" | sed -n 's/^Snapshot: //p' | tail -n 1 | sed 's/^/Snapshot: /'

# mode=exact: quiet and 0 when the trees are equal; otherwise `diff -rq` of
# the two trees, then unified diffs of up to the first three differing files,
# on stderr, and exit 1. mode=ws: report files present on one side only, and
# files whose contents differ once all whitespace is removed (formatting
# newlines between tags count as equal; check suspicious files in exact mode).
# Exits 0 when equal, 1 when different, 2 on usage errors.
# Compare two build outputs.
output-diff a b mode="exact":
    #!/usr/bin/env bash
    set -uo pipefail
    test -d "{{ a }}" || { echo "missing {{ a }}" >&2; exit 2; }
    test -d "{{ b }}" || { echo "missing {{ b }}" >&2; exit 2; }
    case "{{ mode }}" in
      exact)
        # Quiet and 0 when equal. Otherwise name what differs (to stderr, so a
        # runner that drops stdout on failure still shows it) and show the
        # first three differing files as unified diffs.
        names="$(diff -rq "{{ a }}" "{{ b }}")" && exit 0
        {
          echo "$names"
          echo "$names" \
            | sed -n 's/^Files \(.*\) and \(.*\) differ$/\1\t\2/p' \
            | head -n 3 \
            | while IFS=$'\t' read -r fa fb; do
                echo
                diff -u "$fa" "$fb"
              done
        } >&2
        exit 1
        ;;
      ws)
        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT
        (cd "{{ a }}" && find . -type f | sort) > "$tmp/a"
        (cd "{{ b }}" && find . -type f | sort) > "$tmp/b"
        status=0
        diff "$tmp/a" "$tmp/b" || status=1
        while IFS= read -r f; do
          [ -f "{{ b }}/$f" ] || continue
          if ! cmp -s <(tr -d '[:space:]' < "{{ a }}/$f") <(tr -d '[:space:]' < "{{ b }}/$f"); then
            echo "differs beyond whitespace: $f"
            status=1
          fi
        done < "$tmp/a"
        exit "$status"
        ;;
      *)
        echo "mode must be exact or ws" >&2
        exit 2
        ;;
    esac

# Runs the criterion bench `full_build` (publisher_cli/benches/build_bench.rs)
# at the 1K and 10K page corpora, optimised (cargo's bench profile) with the
# dev polylith profile. Each result prints `time: [low median high]`; compare
# the middle value. Slow: about 30 seconds of measurement per size.
# Benchmark a full site build at 1K and 10K pages.
perf:
    cargo polylith cargo --profile dev bench -p publisher_cli --bench build_bench -- 'full_build/pages/(1000|10000)$'

# Runs the same bench as `perf` in a throwaway worktree at git <rev>, like snapshot-ref: the
# worktree is removed afterwards, and it has its own target dir (a full build),
# so the numbers are the rev's and the working tree's build is left alone.
# Compare with `just perf` on the working tree.
# Benchmark a full site build at 1K and 10K pages at a git rev.
perf-ref rev:
    #!/usr/bin/env bash
    set -euo pipefail
    wt="$(mktemp -d -t presemble-perf.XXXXXX)"
    cleanup() { git worktree remove --force "$wt" >/dev/null 2>&1 || rm -rf "$wt"; git worktree prune; }
    trap cleanup EXIT
    git worktree add --detach "$wt" "{{ rev }}" >&2
    # The command is repeated, not `just perf`: the rev's own justfile may
    # predate this recipe.
    (cd "$wt" && cargo polylith cargo --profile dev bench -p publisher_cli --bench build_bench -- 'full_build/pages/(1000|10000)$')

# Run the smoketest (tools/smoketest.sh) against the given polylith profile.
smoketest profile="dev":
    ./tools/smoketest.sh {{ profile }}

# Run workspace tests (dev profile). Prefer the rust-codebase MCP `cargo_test`.
test:
    cargo polylith cargo --profile dev test

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

# --- alpha box (infra/alpha module — separate Tofu state from conductor) ---

# Cross-compile the alpha control-plane binary (Rust) for the Bahnhof box.
# Output: target/x86_64-unknown-linux-gnu/release/control_plane
alpha-build-cp:
    ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig" cargo polylith cargo --profile live zigbuild --release --target x86_64-unknown-linux-gnu.2.41 --bin control_plane

# Cross-compile the presemble binary for the alpha box (site builder / serve).
# Output: target/x86_64-unknown-linux-gnu/release/presemble
alpha-build-presemble:
    ZIG_GLOBAL_CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/zig" cargo polylith cargo --profile live zigbuild --release --target x86_64-unknown-linux-gnu.2.41 --bin presemble

# Cross-compile both alpha binaries.
alpha-build: alpha-build-cp alpha-build-presemble

# Preview changes to the alpha box / network.
# The session key is sourced from alpha-secrets-file just-in-time into -var.
alpha-plan:
    @test -f {{ alpha-secrets-file }} || { echo "missing {{ alpha-secrets-file }} — create it with: printf 'PRESEMBLE_SESSION_KEY=%s\n' \"\$(openssl rand -base64 32)\" > {{ alpha-secrets-file }} && chmod 600 {{ alpha-secrets-file }}"; exit 1; }
    set -a && . {{ alpha-secrets-file }} && set +a && tofu -chdir=infra/alpha plan -var presemble_session_key="$PRESEMBLE_SESSION_KEY"

# Apply the alpha box / network (non-interactive; run alpha-plan first).
alpha-apply:
    @test -f {{ alpha-secrets-file }} || { echo "missing {{ alpha-secrets-file }} — create it with: printf 'PRESEMBLE_SESSION_KEY=%s\n' \"\$(openssl rand -base64 32)\" > {{ alpha-secrets-file }} && chmod 600 {{ alpha-secrets-file }}"; exit 1; }
    set -a && . {{ alpha-secrets-file }} && set +a && tofu -chdir=infra/alpha apply -auto-approve -var presemble_session_key="$PRESEMBLE_SESSION_KEY"

# Re-provision the alpha box from scratch (destroys the instance, re-creates from
# cloud-init). The floating IP re-associates so the public address is unchanged.
alpha-replace:
    @test -f {{ alpha-secrets-file }} || { echo "missing {{ alpha-secrets-file }} — run: just alpha-plan to see the creation message"; exit 1; }
    set -a && . {{ alpha-secrets-file }} && set +a && tofu -chdir=infra/alpha apply -auto-approve -var presemble_session_key="$PRESEMBLE_SESSION_KEY" -replace=openstack_compute_instance_v2.presemble_alpha

# Deploy both binaries to the alpha box and restart the control-plane service.
# Prerequisites: alpha-build + alpha box provisioned (alpha-apply).
alpha-deploy cp_bin="target/x86_64-unknown-linux-gnu/release/control_plane" presemble_bin="target/x86_64-unknown-linux-gnu/release/presemble":
    test -x {{ cp_bin }} || { echo "missing {{ cp_bin }} — run: just alpha-build-cp"; exit 1; }
    test -x {{ presemble_bin }} || { echo "missing {{ presemble_bin }} — run: just alpha-build-presemble"; exit 1; }
    scp -F {{ ssh-config }} {{ cp_bin }} alpha:/tmp/control_plane.new
    scp -F {{ ssh-config }} {{ presemble_bin }} alpha:/tmp/presemble.new
    ssh -F {{ ssh-config }} alpha 'sudo install -o root -g root -m0755 /tmp/control_plane.new /usr/local/bin/control_plane && sudo install -o root -g root -m0755 /tmp/presemble.new /usr/local/bin/presemble && sudo systemctl restart control-plane && rm -f /tmp/control_plane.new /tmp/presemble.new && sleep 1 && systemctl is-active control-plane'

# --- alpha DNS ---

# Preview the alpha DNS records (app + wildcard) against the live alpha floating IP.
dns-plan-alpha:
    @test -f {{ secrets-file }} || { echo "missing {{ secrets-file }} — see infra/dns/README.md"; exit 1; }
    @test -n "{{ _alpha-ip }}" || { echo "alpha floating IP not available — provision the alpha box first (just alpha-apply)"; exit 1; }
    set -a && . {{ secrets-file }} && set +a && tofu -chdir=infra/dns plan -var floating_ip={{ _conductor-ip }} -var alpha_floating_ip={{ _alpha-ip }}

# Apply the alpha DNS records (app.presemble.pub + *.presemble.pub).
# Run dns-plan-alpha first as the review gate.
dns-apply-alpha:
    @test -f {{ secrets-file }} || { echo "missing {{ secrets-file }} — see infra/dns/README.md"; exit 1; }
    @test -n "{{ _alpha-ip }}" || { echo "alpha floating IP not available — provision the alpha box first (just alpha-apply)"; exit 1; }
    set -a && . {{ secrets-file }} && set +a && tofu -chdir=infra/dns apply -auto-approve -var floating_ip={{ _conductor-ip }} -var alpha_floating_ip={{ _alpha-ip }}
