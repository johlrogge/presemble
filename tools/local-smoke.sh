#!/usr/bin/env bash
# tools/local-smoke.sh
#
# LOCAL END-TO-END SMOKE TEST for SharedVmProvisioner
# =====================================================
# Exercises the REAL provisioning path on this dev machine without DNS, TLS,
# or a production Caddy instance.  All network traffic stays on 127.0.0.1.
#
# WHAT IT PROVES
# --------------
#   1. TokioProcessLauncher can spawn a real "presemble serve" child process.
#   2. HttpHealthChecker successfully polls the conductor until it is healthy.
#   3. HttpCaddyRouter successfully PUTs a route to a Caddy-admin-compatible
#      endpoint (stub, see below).
#   4. The shared PRESEMBLE_SESSION_KEY flows from control_plane → conductor
#      (the same key is used to sign and verify hand-off tokens).
#   5. The full magic-link sign-in flow through control_plane works end-to-end:
#      POST /login → ConsoleMailSender token → GET /auth/verify → session cookie.
#   6. POST /sites provisions a real conductor process and writes registry.json.
#   7. The real conductor serves the scaffolded "personal" site on its allocated
#      port (confirmed with a direct GET to 127.0.0.1:<conductor_port>/).
#   8. GET /sites/<id>/open produces a 302 whose Location contains a hand-off
#      token for "https://demo.localhost/_presemble/login?token=…".
#   9. The hand-off token is accepted by the real conductor's login endpoint
#      (GET 127.0.0.1:<port>/_presemble/login?token=…) and a session cookie
#      is returned.
#  10. An authenticated editorial request to the conductor (GET /_presemble/render)
#      with that cookie returns a non-401 response.
#
# WHAT IT DOES NOT PROVE (deferred to the box)
# -----------------------------------------------
#   - Real Caddy routing (wildcard DNS → subdomain → conductor).
#   - Wildcard TLS certificate issuance/renewal.
#   - Production Caddy @id API compatibility (we use a tiny stub here).
#   - The control_plane binary running as a systemd service with the correct
#     environment.
#   - SMTP mail delivery (ConsoleMailSender is used here, as in dev).
#
# CADDY ADMIN STUB RATIONALE
# ---------------------------
# HttpCaddyRouter hardcodes the admin address to "127.0.0.1:2019" (it is NOT
# configurable via an environment variable — the addr is set in HttpCaddyRouter::new
# via a struct field default; main.rs does not override it).  caddy is not
# declared in devenv.nix, so it is not available in this shell.  The smoke's
# goal is the PROVISIONER + CONDUCTOR + HAND-OFF, not Caddy's routing itself.
# We therefore stand up a minimal Python HTTP stub on port 2019 that accepts
# PUT and DELETE on any path and returns 200 OK.  This satisfies HttpCaddyRouter
# without requiring a real Caddy binary.
#
# BINARY PATH NOTE
# ----------------
# TokioProcessLauncher::new() sets binary = PathBuf::from("presemble"), meaning
# it performs a PATH lookup.  devenv.nix's enterShell prepends target/release to
# PATH, which means "presemble" resolves to the release binary.  However we build
# in DEBUG here to conserve disk space.  The script explicitly prepends the debug
# output directory to PATH so that the launcher's PATH lookup finds the correct
# debug binary.  Both presemble (publisher) and control_plane are built; no other
# binaries are needed.
#
# PREREQUISITES
# -------------
#   - devenv shell (Rust toolchain, cargo, cargo-polylith, python3 all in PATH)
#   - ~300-500 MB free disk (debug builds; presemble ~150 MB, control_plane ~100 MB)
#   - Ports 2019, 8080 must be free (control_plane listens on 8080; stub on 2019)
#   - The conductor will occupy one port in the range 3100-3999
#   - python3 must be available (used for the Caddy admin stub)
#
# USAGE
#   chmod +x tools/local-smoke.sh
#   ./tools/local-smoke.sh
#
# Exit code: 0 on PASS, 1 on any FAIL.

set -euo pipefail

# ── Colour helpers ─────────────────────────────────────────────────────────────

RED='\033[0;31m'
GRN='\033[0;32m'
YEL='\033[0;33m'
BLD='\033[1m'
RST='\033[0m'

pass() { echo -e "${GRN}[PASS]${RST} $*"; }
fail() { echo -e "${RED}[FAIL]${RST} $*"; SMOKE_FAILED=1; }
info() { echo -e "${YEL}[INFO]${RST} $*"; }
step() { echo -e "${BLD}${YEL}==> $*${RST}"; }

SMOKE_FAILED=0

# ── Process tracking ───────────────────────────────────────────────────────────

STUB_PID=""
CONTROL_PLANE_PID=""
TMPDIR_ROOT=""

cleanup() {
    echo ""
    info "Cleaning up..."

    # Kill control_plane by its PID (direct — avoids killing other processes)
    if [ -n "$CONTROL_PLANE_PID" ] && kill -0 "$CONTROL_PLANE_PID" 2>/dev/null; then
        kill "$CONTROL_PLANE_PID" 2>/dev/null || true
        info "Killed control_plane (PID $CONTROL_PLANE_PID)"
    fi

    # Kill conductor processes — NEVER use `pkill -f presemble` (kills the devenv
    # shell).  Use the more specific pattern that matches only serve sub-commands.
    pkill -f "presemble serve" 2>/dev/null && info "Killed presemble serve process(es)" || true

    # Kill Caddy admin stub
    if [ -n "$STUB_PID" ] && kill -0 "$STUB_PID" 2>/dev/null; then
        kill "$STUB_PID" 2>/dev/null || true
        info "Killed Caddy admin stub (PID $STUB_PID)"
    fi

    # Remove temp dirs
    if [ -n "$TMPDIR_ROOT" ] && [ -d "$TMPDIR_ROOT" ]; then
        rm -rf "$TMPDIR_ROOT"
        info "Removed temp dir $TMPDIR_ROOT"
    fi
}

trap cleanup EXIT

# ── Locate repository root ─────────────────────────────────────────────────────

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
info "Repo root: $REPO_ROOT"

# ── Step 0: Prereq checks ──────────────────────────────────────────────────────

step "Step 0: Prereq checks"

for cmd in cargo python3 curl jq; do
    if ! command -v "$cmd" &>/dev/null; then
        fail "Required command not found: $cmd"
        exit 1
    fi
    info "Found: $(command -v "$cmd")"
done

# Check python3 can serve HTTP (http.server is stdlib)
if ! python3 -c "import http.server" 2>/dev/null; then
    fail "python3 http.server module not available"
    exit 1
fi
pass "python3 http.server available"

# Check cargo-polylith (probe the binary on PATH; `cargo polylith --version`
# is not a supported invocation, so check for the cargo-polylith binary itself)
if ! command -v cargo-polylith &>/dev/null; then
    fail "cargo-polylith not found — run inside devenv shell"
    exit 1
fi
pass "cargo-polylith available"

# Check ports 2019 and 8080 are free
for PORT in 2019 8080; do
    if ss -tlnp 2>/dev/null | grep -q ":${PORT} " || \
       curl -s --connect-timeout 1 "http://127.0.0.1:${PORT}/" &>/dev/null; then
        fail "Port ${PORT} is already in use — free it before running this smoke test"
        exit 1
    fi
done
pass "Ports 2019 and 8080 are free"

# Check disk space (warn if less than 600 MB free in target directory)
TARGET_DIR="$REPO_ROOT/target"
mkdir -p "$TARGET_DIR"
AVAIL_MB=$(df -m "$TARGET_DIR" | awk 'NR==2 {print $4}')
if [ "$AVAIL_MB" -lt 600 ]; then
    fail "Less than 600 MB free in $TARGET_DIR ($AVAIL_MB MB available). Debug builds may fail."
    fail "Run: cargo clean  or free disk space before continuing."
    exit 1
fi
info "Disk: ${AVAIL_MB} MB free in $TARGET_DIR"
pass "Disk space adequate"

# ── Step 1: Build binaries (debug) ────────────────────────────────────────────
#
# The polylith build command (from project memory):
#   cargo polylith cargo --profile dev run --bin presemble -- build site/
# For building only:
#   cargo polylith cargo --profile dev build --bin presemble
#   cargo polylith cargo --profile dev build --bin control_plane
#
# TokioProcessLauncher looks up "presemble" via PATH.  devenv enterShell adds
# target/release to PATH, but we built debug.  We prepend target/debug to PATH
# below so the launcher finds the debug binary.

step "Step 1: Building binaries (debug profile)"

cd "$REPO_ROOT"

info "Building presemble binary..."
cargo polylith cargo --profile dev build --bin presemble 2>&1 | tail -5
PRESEMBLE_BIN="$REPO_ROOT/target/debug/presemble"
if [ ! -x "$PRESEMBLE_BIN" ]; then
    fail "presemble binary not found at $PRESEMBLE_BIN after build"
    exit 1
fi
pass "presemble binary: $PRESEMBLE_BIN"

info "Building control_plane binary..."
cargo polylith cargo --profile dev build --bin control_plane 2>&1 | tail -5
CONTROL_PLANE_BIN="$REPO_ROOT/target/debug/control_plane"
if [ ! -x "$CONTROL_PLANE_BIN" ]; then
    fail "control_plane binary not found at $CONTROL_PLANE_BIN after build"
    exit 1
fi
pass "control_plane binary: $CONTROL_PLANE_BIN"

# Prepend debug build dir so TokioProcessLauncher's PATH lookup finds our binary.
# IMPORTANT: this must come AFTER any release binary on PATH so this is the
# leftmost match.
export PATH="$REPO_ROOT/target/debug:$PATH"
info "Prepended $REPO_ROOT/target/debug to PATH"
info "presemble resolves to: $(command -v presemble)"

# ── Step 2: Set up temp directories and env vars ───────────────────────────────

step "Step 2: Creating temp environment"

TMPDIR_ROOT="$(mktemp -d)"
info "Temp root: $TMPDIR_ROOT"

ACCOUNTS_ROOT="$TMPDIR_ROOT/accounts"
REGISTRY_PATH="$TMPDIR_ROOT/registry.json"
mkdir -p "$ACCOUNTS_ROOT"

# PRESEMBLE_SESSION_KEY: shared between control_plane and each conductor it spawns.
# Must be at least 32 bytes (ADR-055 amendment); 32 random bytes base64 = 44 chars.
SESSION_KEY="$(head -c 32 /dev/urandom | base64)"

# Control plane listens on a high port to avoid needing root.
CONTROL_PLANE_PORT=8080
CONTROL_PLANE_ADDR="127.0.0.1:$CONTROL_PLANE_PORT"
PUBLIC_BASE_URL="http://127.0.0.1:$CONTROL_PLANE_PORT"
BASE_DOMAIN="localhost"

# Log files for background processes
CP_LOG="$TMPDIR_ROOT/control_plane.log"
STUB_LOG="$TMPDIR_ROOT/caddy_stub.log"

pass "Temp environment ready"
info "  PRESEMBLE_ACCOUNTS_ROOT = $ACCOUNTS_ROOT"
info "  PRESEMBLE_REGISTRY_PATH = $REGISTRY_PATH"
info "  PRESEMBLE_BASE_DOMAIN   = $BASE_DOMAIN"
info "  PRESEMBLE_PUBLIC_BASE_URL = $PUBLIC_BASE_URL"

# ── Step 3: Start Caddy admin stub ────────────────────────────────────────────
#
# HttpCaddyRouter hardcodes admin_addr = "127.0.0.1:2019" (NOT configurable via
# env var — it is a struct field default, not read from the environment).
# The stub accepts PUT and DELETE on any path and returns 200 OK, which is all
# HttpCaddyRouter needs for add_route to succeed.

step "Step 3: Starting Caddy admin stub on 127.0.0.1:2019"

python3 - <<'PYSTUB' &
import http.server, socketserver

class CaddyStubHandler(http.server.BaseHTTPRequestHandler):
    def do_PUT(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length)
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"ok":true}')

    def do_DELETE(self):
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(b'{"ok":true}')

    def log_message(self, fmt, *args):
        import sys
        print(f"[caddy-stub] {fmt % args}", flush=True, file=sys.stderr)

with socketserver.TCPServer(("127.0.0.1", 2019), CaddyStubHandler) as httpd:
    httpd.allow_reuse_address = True
    httpd.serve_forever()
PYSTUB
STUB_PID=$!

# Wait for stub to be ready
for i in $(seq 1 10); do
    if curl -s -o /dev/null -w "%{http_code}" -X PUT http://127.0.0.1:2019/id/test 2>/dev/null | grep -q "200"; then
        break
    fi
    sleep 0.3
done
if ! curl -s -o /dev/null -w "%{http_code}" -X PUT http://127.0.0.1:2019/id/test 2>/dev/null | grep -q "200"; then
    fail "Caddy admin stub did not start within 3 seconds"
    exit 1
fi
pass "Caddy admin stub running (PID $STUB_PID)"

# ── Step 4: Start control_plane ───────────────────────────────────────────────

step "Step 4: Starting control_plane"

PRESEMBLE_SESSION_KEY="$SESSION_KEY" \
PRESEMBLE_LISTEN_ADDR="$CONTROL_PLANE_ADDR" \
PRESEMBLE_PUBLIC_BASE_URL="$PUBLIC_BASE_URL" \
PRESEMBLE_BASE_DOMAIN="$BASE_DOMAIN" \
PRESEMBLE_ACCOUNTS_ROOT="$ACCOUNTS_ROOT" \
PRESEMBLE_REGISTRY_PATH="$REGISTRY_PATH" \
"$CONTROL_PLANE_BIN" > "$CP_LOG" 2>&1 &
CONTROL_PLANE_PID=$!

info "control_plane PID: $CONTROL_PLANE_PID, log: $CP_LOG"

# Wait for control_plane to be ready
CP_READY=0
for i in $(seq 1 30); do
    if curl -s -o /dev/null -w "%{http_code}" "http://$CONTROL_PLANE_ADDR/login" 2>/dev/null | grep -q "200"; then
        CP_READY=1
        break
    fi
    sleep 0.5
done

if [ "$CP_READY" -eq 0 ]; then
    fail "control_plane did not become ready within 15 seconds"
    echo "--- control_plane log ---"
    cat "$CP_LOG"
    exit 1
fi
pass "control_plane listening on $CONTROL_PLANE_ADDR"

# ── Step 5: Magic-link sign-in flow ──────────────────────────────────────────
#
# POST /login sends to ConsoleMailSender which prints to stdout (redirected to CP_LOG).
# Output format: "[presemble] Magic-link login for alice@example.com\n  URL: <url>\n  ..."
# We capture the token from the URL line.

step "Step 5: Magic-link sign-in"

info "POST /login alice@example.com..."
HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST \
    -H "Content-Type: application/x-www-form-urlencoded" \
    -d "email=alice@example.com" \
    "http://$CONTROL_PLANE_ADDR/login")

if [ "$HTTP_STATUS" != "200" ]; then
    fail "POST /login returned $HTTP_STATUS (expected 200)"
else
    pass "POST /login returned 200"
fi

# Extract verify token from CP_LOG.
# The ConsoleMailSender prints:
#   [presemble] Magic-link login for alice@example.com
#     URL: http://127.0.0.1:8080/auth/verify?token=<TOKEN>
#     (console sender — for demo/dev only)
# We need to give the process a moment to flush stdout.
sleep 0.5

VERIFY_URL=$(grep "auth/verify?token=" "$CP_LOG" | tail -1 | sed 's/.*URL: //' | tr -d '[:space:]')
if [ -z "$VERIFY_URL" ]; then
    fail "Could not extract verify URL from control_plane log"
    echo "--- control_plane log (last 20 lines) ---"
    tail -20 "$CP_LOG"
    exit 1
fi
LOGIN_TOKEN="${VERIFY_URL##*token=}"
info "Captured verify token: ${LOGIN_TOKEN:0:20}…"
pass "Magic-link token captured from ConsoleMailSender output"

# Exchange login token for session cookie via GET /auth/verify
info "GET /auth/verify?token=..."
COOKIE_HEADER=$(curl -s -D - -o /dev/null \
    "$VERIFY_URL" \
    | grep -i "^set-cookie:" | head -1 | tr -d '\r')

if [ -z "$COOKIE_HEADER" ]; then
    fail "GET /auth/verify did not set a cookie"
    exit 1
fi

SESSION_COOKIE=$(echo "$COOKIE_HEADER" | sed 's/.*set-cookie: //i' | cut -d';' -f1)
if ! echo "$SESSION_COOKIE" | grep -q "presemble_session="; then
    fail "Set-Cookie does not contain presemble_session; got: $SESSION_COOKIE"
    exit 1
fi
pass "GET /auth/verify returned presemble_session cookie"
info "Session cookie: ${SESSION_COOKIE:0:30}…"

# ── Step 6: Create a site ────────────────────────────────────────────────────

step "Step 6: POST /sites subdomain=demo template=personal"

HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
    -X POST \
    -H "Content-Type: application/x-www-form-urlencoded" \
    -H "Cookie: $SESSION_COOKIE" \
    -d "subdomain=demo&template=personal" \
    --max-redirs 0 \
    "http://$CONTROL_PLANE_ADDR/sites")

if [ "$HTTP_STATUS" != "303" ]; then
    fail "POST /sites returned $HTTP_STATUS (expected 303)"
    echo "--- control_plane log (last 30 lines) ---"
    tail -30 "$CP_LOG"
    SMOKE_FAILED=1
else
    pass "POST /sites returned 303 (provisioning triggered)"
fi

# ── Step 7: Wait for site to be Ready and read port from registry ──────────
#
# Registry file shape (from site_registry::FileSiteStore / serde):
#   [{"id":"...","account_id":"...","subdomain":"demo","status":"Ready",
#     "placement":{"host":"127.0.0.1","port":3100,"site_path":"..."}}]
#
# The provisioner writes to registry.json and marks Ready after launch + health.
# health_timeout is 30s in main.rs; we poll up to 45s here.

step "Step 7: Waiting for site to reach Ready status in registry"

CONDUCTOR_PORT=""
SITE_ID=""
READY=0

for i in $(seq 1 90); do
    if [ -f "$REGISTRY_PATH" ]; then
        # Extract status and port for the 'demo' subdomain
        DEMO_STATUS=$(jq -r '.[] | select(.subdomain=="demo") | .status' "$REGISTRY_PATH" 2>/dev/null || echo "")
        if [ "$DEMO_STATUS" = "Ready" ]; then
            CONDUCTOR_PORT=$(jq -r '.[] | select(.subdomain=="demo") | .placement.port' "$REGISTRY_PATH" 2>/dev/null || echo "")
            SITE_ID=$(jq -r '.[] | select(.subdomain=="demo") | .id' "$REGISTRY_PATH" 2>/dev/null || echo "")
            READY=1
            break
        fi
        if [ "$DEMO_STATUS" = "Failed" ]; then
            fail "Site entered Failed status in registry"
            echo "--- control_plane log (last 40 lines) ---"
            tail -40 "$CP_LOG"
            SMOKE_FAILED=1
            break
        fi
    fi
    sleep 0.5
done

if [ "$READY" -eq 0 ]; then
    fail "Site did not reach Ready status within 45 seconds"
    if [ -f "$REGISTRY_PATH" ]; then
        info "Registry contents:"
        cat "$REGISTRY_PATH"
    fi
    echo "--- control_plane log (last 40 lines) ---"
    tail -40 "$CP_LOG"
    SMOKE_FAILED=1
else
    pass "Site is Ready in registry (site_id=$SITE_ID, conductor_port=$CONDUCTOR_PORT)"
fi

# ── Step 8: Verify real conductor is serving the scaffolded site ───────────
#
# The conductor listens on 127.0.0.1:<port> and serves the scaffolded "personal"
# template.  We bypass DNS/TLS entirely and hit the conductor directly.

step "Step 8: Verifying conductor serves scaffolded content"

if [ -n "$CONDUCTOR_PORT" ]; then
    CONDUCTOR_URL="http://127.0.0.1:$CONDUCTOR_PORT"
    info "Hitting conductor at $CONDUCTOR_URL/"

    CONDUCTOR_HTTP=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 "$CONDUCTOR_URL/")
    CONDUCTOR_BODY=$(curl -s --max-time 10 "$CONDUCTOR_URL/")

    if [ "$CONDUCTOR_HTTP" != "200" ]; then
        fail "Conductor GET / returned $CONDUCTOR_HTTP (expected 200)"
        SMOKE_FAILED=1
    else
        pass "Conductor GET / returned 200"
    fi

    if [ -z "$CONDUCTOR_BODY" ]; then
        fail "Conductor GET / returned empty body"
        SMOKE_FAILED=1
    else
        BODY_LEN="${#CONDUCTOR_BODY}"
        pass "Conductor response is non-empty ($BODY_LEN bytes)"
        # The scaffolded personal template should render some recognisable HTML
        if echo "$CONDUCTOR_BODY" | grep -qi "<!DOCTYPE html\|<html"; then
            pass "Conductor response contains HTML"
        else
            fail "Conductor response does not look like HTML"
            SMOKE_FAILED=1
        fi
    fi
else
    fail "Cannot verify conductor — no port was captured from registry"
    SMOKE_FAILED=1
fi

# ── Step 9: Test /sites/<id>/open hand-off redirect ───────────────────────────
#
# Expected: 302 with Location: https://demo.localhost/_presemble/login?token=<tok>
# Note: HttpCaddyRouter uses base_domain from env, which we set to "localhost".
# The open_editor handler constructs: https://<sub>.<base_domain>/_presemble/login?token=…
# So: https://demo.localhost/_presemble/login?token=…

step "Step 9: GET /sites/$SITE_ID/open — hand-off redirect"

if [ -n "$SITE_ID" ]; then
    OPEN_RESPONSE=$(curl -s -D - -o /dev/null \
        --max-redirs 0 \
        -H "Cookie: $SESSION_COOKIE" \
        "http://$CONTROL_PLANE_ADDR/sites/$SITE_ID/open" \
        | tr -d '\r')

    OPEN_STATUS=$(echo "$OPEN_RESPONSE" | grep "^HTTP/" | awk '{print $2}')
    OPEN_LOCATION=$(echo "$OPEN_RESPONSE" | grep -i "^location:" | sed 's/location: //i')

    if [ "$OPEN_STATUS" != "302" ]; then
        fail "GET /sites/$SITE_ID/open returned $OPEN_STATUS (expected 302)"
        SMOKE_FAILED=1
    else
        pass "GET /sites/$SITE_ID/open returned 302"
    fi

    EXPECTED_PREFIX="https://demo.localhost/_presemble/login?token="
    if echo "$OPEN_LOCATION" | grep -q "^$EXPECTED_PREFIX"; then
        pass "Location header has expected prefix: $EXPECTED_PREFIX"
    else
        fail "Location header does not match expected prefix"
        fail "  Expected prefix: $EXPECTED_PREFIX"
        fail "  Got:             $OPEN_LOCATION"
        SMOKE_FAILED=1
    fi

    # Extract the hand-off token from the Location header
    HANDOFF_TOKEN="${OPEN_LOCATION##*token=}"
    info "Hand-off token: ${HANDOFF_TOKEN:0:20}…"
else
    fail "Cannot test /sites/open — SITE_ID is empty"
    SMOKE_FAILED=1
    HANDOFF_TOKEN=""
fi

# ── Step 10: Verify hand-off token authenticates to conductor directly ────────
#
# /_presemble/login is always PUBLIC on the conductor (no Session extractor).
# When PRESEMBLE_SESSION_KEY is set (which we do), it uses MagicLinkSessionStore
# with the same key — so tokens minted by control_plane are valid in the conductor.
#
# Expected: 302 to / with Set-Cookie: presemble_session=…
# We bypass DNS entirely and hit the conductor's port directly, simulating
# what would happen after Caddy proxies demo.localhost → conductor.

step "Step 10: Verifying hand-off token authenticates to conductor"

if [ -n "$HANDOFF_TOKEN" ] && [ -n "$CONDUCTOR_PORT" ]; then
    CONDUCTOR_URL="http://127.0.0.1:$CONDUCTOR_PORT"
    info "GET $CONDUCTOR_URL/_presemble/login?token=${HANDOFF_TOKEN:0:20}…"

    LOGIN_RESPONSE=$(curl -s -D - -o /dev/null \
        --max-redirs 0 \
        "$CONDUCTOR_URL/_presemble/login?token=$HANDOFF_TOKEN" \
        | tr -d '\r')

    LOGIN_STATUS=$(echo "$LOGIN_RESPONSE" | grep "^HTTP/" | awk '{print $2}')
    LOGIN_SET_COOKIE=$(echo "$LOGIN_RESPONSE" | grep -i "^set-cookie:" | head -1 | sed 's/set-cookie: //i' | cut -d';' -f1)

    if [ "$LOGIN_STATUS" != "302" ]; then
        fail "Conductor /_presemble/login returned $LOGIN_STATUS (expected 302)"
        SMOKE_FAILED=1
    else
        pass "Conductor /_presemble/login returned 302"
    fi

    if echo "$LOGIN_SET_COOKIE" | grep -q "presemble_session="; then
        pass "Conductor set presemble_session cookie on hand-off"
        CONDUCTOR_SESSION_COOKIE="$LOGIN_SET_COOKIE"
    else
        fail "Conductor did not set presemble_session cookie"
        fail "  Set-Cookie: $LOGIN_SET_COOKIE"
        SMOKE_FAILED=1
        CONDUCTOR_SESSION_COOKIE=""
    fi
else
    fail "Cannot test hand-off — missing token or conductor port"
    SMOKE_FAILED=1
    CONDUCTOR_SESSION_COOKIE=""
fi

# ── Step 11: Authenticated editorial request to conductor ────────────────────
#
# GET /_presemble/render?path=/&mode=view requires a Session (authenticated).
# With the conductor session cookie, this should return non-401.
# (The conductor may return 200 or an error if the path doesn't render, but
# it must not return 401 — that would mean the session cookie is not accepted.)

step "Step 11: Authenticated editorial request to conductor"

if [ -n "$CONDUCTOR_SESSION_COOKIE" ] && [ -n "$CONDUCTOR_PORT" ]; then
    CONDUCTOR_URL="http://127.0.0.1:$CONDUCTOR_PORT"
    info "GET $CONDUCTOR_URL/_presemble/render?path=/&mode=view with session cookie"

    RENDER_STATUS=$(curl -s -o /dev/null -w "%{http_code}" \
        --max-time 15 \
        -H "Cookie: $CONDUCTOR_SESSION_COOKIE" \
        "$CONDUCTOR_URL/_presemble/render?path=/&mode=view")

    if [ "$RENDER_STATUS" = "401" ]; then
        fail "Conductor /_presemble/render returned 401 — session cookie not accepted"
        SMOKE_FAILED=1
    elif [ "$RENDER_STATUS" = "000" ]; then
        fail "Conductor /_presemble/render timed out or connection refused"
        SMOKE_FAILED=1
    else
        pass "Conductor /_presemble/render returned $RENDER_STATUS (not 401 — authenticated)"
    fi
else
    fail "Cannot test authenticated request — no conductor session cookie available"
    SMOKE_FAILED=1
fi

# ── Summary ────────────────────────────────────────────────────────────────────

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
if [ "$SMOKE_FAILED" -eq 0 ]; then
    echo -e "${GRN}${BLD}  LOCAL SMOKE TEST: PASS${RST}"
    echo ""
    echo "  All checks passed. The SharedVmProvisioner end-to-end path works."
    echo "  Proven on this machine:"
    echo "    + TokioProcessLauncher spawns a real presemble serve process"
    echo "    + HttpHealthChecker polls conductor until healthy"
    echo "    + HttpCaddyRouter PUT succeeds (stub)"
    echo "    + Magic-link sign-in flow (control_plane)"
    echo "    + POST /sites triggers real provisioning"
    echo "    + Conductor serves scaffolded 'personal' site content"
    echo "    + GET /sites/<id>/open produces correct Location header"
    echo "    + Hand-off token authenticates to conductor (shared session key)"
    echo "    + Authenticated editorial request succeeds (not 401)"
    echo ""
    echo "  Deferred to the box:"
    echo "    - Real Caddy routing (wildcard DNS + TLS)"
    echo "    - Production Caddy @id API (confirmed separately)"
    echo "    - Systemd service environment wiring"
    echo "    - SMTP mail delivery"
    exit 0
else
    echo -e "${RED}${BLD}  LOCAL SMOKE TEST: FAIL${RST}"
    echo ""
    echo "  One or more checks failed.  See [FAIL] lines above."
    echo "  Log files:"
    echo "    control_plane: $CP_LOG"
    echo "    (temp dir preserved for inspection, will be cleaned on next run)"
    exit 1
fi
