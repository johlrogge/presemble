#!/usr/bin/env bash
# Presemble smoke test — exercises the full workflow via curl and rep.
# Usage: ./tools/smoketest.sh [dev|live]
# Default: dev profile
#
# The whole run is also written to /tmp/presemble-smoketest.log (override with
# SMOKETEST_LOG). This is a workaround for metadev#28: the `just` MCP returns
# only the exit status of a failed recipe, not its output, so callers read the
# log instead.

set -euo pipefail

# Keep a copy of the whole run: callers that only see the exit status (the
# `just` MCP) can read the log. Override the path with SMOKETEST_LOG.
exec > >(tee "${SMOKETEST_LOG:-/tmp/presemble-smoketest.log}") 2>&1

PROFILE="${1:-dev}"
PORT=3000
SITE_DIR=""
COOKIE_JAR=""
WS_ERR=""
PID=""
KEY_CHECK_DIR=""
REPRO_A=""
REPRO_B=""
PASS=0
FAIL=0
ERRORS=""

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
NC='\033[0m'

cleanup() {
    if [ -n "$PID" ]; then
        kill "$PID" 2>/dev/null || true
        wait "$PID" 2>/dev/null || true
    fi
    # Kill any conductor processes for our site dir
    if [ -n "$SITE_DIR" ]; then
        pkill -f "presemble conductor $SITE_DIR" 2>/dev/null || true
        pkill -f "presemble serve $SITE_DIR" 2>/dev/null || true
        rm -rf "$SITE_DIR"
    fi
    [ -n "$COOKIE_JAR" ] && rm -f "$COOKIE_JAR"
    [ -n "$WS_ERR" ] && rm -f "$WS_ERR"
    [ -n "$KEY_CHECK_DIR" ] && rm -rf "$KEY_CHECK_DIR"
    [ -n "$REPRO_A" ] && rm -rf "$REPRO_A"
    [ -n "$REPRO_B" ] && rm -rf "$REPRO_B"
    # Clean up nrepl port file
    rm -f "$(dirname "${SITE_DIR:-/tmp/x}")/.nrepl-port" 2>/dev/null || true
}
trap cleanup EXIT

log() { echo -e "${YELLOW}▸${NC} $1"; }
pass() { PASS=$((PASS + 1)); echo -e "  ${GREEN}✓${NC} $1"; }
fail() { FAIL=$((FAIL + 1)); ERRORS="${ERRORS}\n  ✗ $1"; echo -e "  ${RED}✗${NC} $1"; }

# Reclaim port $PORT from smoketest runs that leaked (e.g. SIGKILL'd before
# cleanup ran). Only processes serving a smoketest temp dir are touched —
# a user's own `presemble serve` is never killed.
if curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
    log "Port $PORT is busy — killing leaked smoketest processes..."
    pkill -f "presemble (serve|conductor) /tmp/presemble-smoke-" 2>/dev/null || true
    sleep 2
    if curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
        echo -e "${RED}Port $PORT is in use by something other than a smoketest run (your own 'presemble serve'?). Stop it and re-run.${NC}" >&2
        exit 1
    fi
fi

# ── Static check: browser JS resources parse ───────────────────────────────
# Catches JS syntax errors before we spend a minute building presemble.
# Add new JS resources to BROWSER_JS_FILES below.

BROWSER_JS_FILES=(
    "resources/serve/inject.js"
    "resources/serve/reload.js"
)

log "Static-checking browser JS resources..."
if ! command -v deno &> /dev/null; then
    fail "deno not available (required for JS parse check — add pkgs.deno to devenv.nix and re-enter shell)"
    echo -e "══════════════════════════════════════"
    echo -e "  ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}"
    echo -e "${RED}Failures:${ERRORS}${NC}"
    echo -e "══════════════════════════════════════"
    exit 1
fi

JS_CHECK_FAILED=0
for js_file in "${BROWSER_JS_FILES[@]}"; do
    # Capture, then grep a herestring: under pipefail a pipe into `grep -q` reads
    # deno's own failure exit (or a SIGPIPE) as "no match".
    DENO_OUT=$(deno check "$js_file" 2>&1 || true)
    printf '%s\n' "$DENO_OUT" > /tmp/presemble-deno-check.log
    if grep -qE '^(error|TS[0-9]+)' <<< "$DENO_OUT"; then
        fail "deno check $js_file"
        cat /tmp/presemble-deno-check.log
        JS_CHECK_FAILED=1
    else
        pass "deno check $js_file"
    fi
done

# Deno smoke tools (run later against the live server) — kept apart from the
# browser resources above.
DENO_TOOL_FILES=(
    "tools/ws-smoke.ts"
    "tools/text-vectors.ts"
    "tools/protocol-fixtures.ts"
    "tools/block-edit.ts"
)

for ts_file in "${DENO_TOOL_FILES[@]}"; do
    DENO_OUT=$(deno check "$ts_file" 2>&1 || true)
    printf '%s\n' "$DENO_OUT" > /tmp/presemble-deno-check.log
    if grep -qE '^(error|TS[0-9]+)' <<< "$DENO_OUT"; then
        fail "deno check $ts_file"
        cat /tmp/presemble-deno-check.log
        JS_CHECK_FAILED=1
    else
        pass "deno check $ts_file"
    fi
done

# The browser's pure text helpers (inject.js) must agree with the server on
# the shared vectors in resources/serve/text-vectors.json (ADR-063).
if deno run --no-prompt --allow-read=. tools/text-vectors.ts 2>&1 | tee /tmp/presemble-text-vectors.log; then
    pass "text vectors: inject.js agrees with the server"
else
    fail "text vectors: inject.js disagrees with the server"
    JS_CHECK_FAILED=1
fi

# The browser's block-edit request builder (inject.js) must produce exactly the
# requests in resources/serve/protocol/*.json, which the server is tested
# against too (ADR-059 section 10).
PF_RC=0
PF_OUT=$(deno run --no-prompt --allow-read=. tools/protocol-fixtures.ts 2>&1) || PF_RC=$?
if [ "$PF_RC" -ne 0 ] || ! grep -q '^PASS' <<< "$PF_OUT"; then
    echo "$PF_OUT"
    fail "protocol fixtures: inject.js builds different requests than the fixtures (exit $PF_RC)"
    JS_CHECK_FAILED=1
else
    pass "protocol fixtures: inject.js builds the fixture requests ($(grep -c '^PASS' <<< "$PF_OUT") fixtures)"
fi

# Fixtures that are not browser requests (EDN text) are skipped, never checked.
for pf in resources/serve/protocol/*-edn.json; do
    pf_name=$(basename "$pf" .json)
    if grep -q "^SKIP $pf_name " <<< "$PF_OUT" && ! grep -q "^PASS $pf_name\$" <<< "$PF_OUT"; then
        pass "protocol fixtures: $pf_name is skipped (not a browser request)"
    else
        fail "protocol fixtures: $pf_name must be skipped, not checked"
        JS_CHECK_FAILED=1
    fi
done

if [ $JS_CHECK_FAILED -eq 1 ]; then
    echo -e "══════════════════════════════════════"
    echo -e "  ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}"
    echo -e "${RED}Failures:${ERRORS}${NC}"
    echo -e "══════════════════════════════════════"
    exit 1
fi

# All editorial requests go through here so the session cookie is sent.
acurl() { curl -s -b "$COOKIE_JAR" "$@"; }

# Apply one block edit as the browser does (tools/block-edit.ts): read the
# editorial page, take the address and markdown of the slot's element (the
# placeholder of an empty slot has empty markdown), build the request with
# inject.js's own builder, POST it to /_presemble/block-edit. Passes when the
# server says it applied the edit.
block_edit() {
    local desc="$1" page="$2" file="$3" slot="$4" proposed="$5" out
    local cookie
    cookie=$(awk '$6=="presemble_session"{print $7}' "$COOKIE_JAR")
    out=$(PRESEMBLE_SMOKE_BASE="http://127.0.0.1:$PORT" PRESEMBLE_SMOKE_COOKIE="$cookie" \
        deno run --no-prompt --allow-net=127.0.0.1:$PORT \
        --allow-env=PRESEMBLE_SMOKE_BASE,PRESEMBLE_SMOKE_COOKIE \
        --allow-read=resources/serve/inject.js \
        tools/block-edit.ts "$page" "$file" "$slot" "$proposed" 2>&1 || true)
    if grep -qF '"applied"' <<< "$out"; then
        pass "$desc"
    else
        fail "$desc (got: $out)"
    fi
}

# Assert a curl response contains expected text
assert_curl() {
    local desc="$1" url="$2" method="${3:-GET}" body="${4:-}" expected="$5"
    local response
    if [ "$method" = "POST" ]; then
        response=$(acurl -X POST "http://127.0.0.1:$PORT$url" -H 'Content-Type: application/json' -d "$body" 2>&1)
    else
        response=$(acurl "http://127.0.0.1:$PORT$url" 2>&1)
    fi
    # Use herestring (<<<) instead of pipe to avoid SIGPIPE: with `set -o pipefail`,
    # `echo "$response" | grep -qF` would propagate a SIGPIPE failure when grep finds
    # an early match and closes the pipe before echo finishes writing a large response.
    if grep -qF "$expected" <<< "$response"; then
        pass "$desc"
    else
        fail "$desc (expected '$expected', got: $response)"
    fi
}

# Assert a curl response does NOT contain a string
assert_curl_not_contains() {
    local desc="$1" url="$2" unexpected="$3"
    local response
    response=$(acurl "http://127.0.0.1:$PORT$url" 2>&1)
    if grep -qF "$unexpected" <<< "$response"; then
        fail "$desc (unexpected '$unexpected' found in: $response)"
    else
        pass "$desc"
    fi
}

# Assert HTTP status code
assert_status() {
    local desc="$1" url="$2" expected="$3"
    local status
    status=$(acurl -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT$url" 2>&1)
    if [ "$status" = "$expected" ]; then
        pass "$desc"
    else
        fail "$desc (expected HTTP $expected, got $status)"
    fi
}

# Assert HTTP status code WITHOUT the session cookie (auth boundary checks).
assert_status_anon() {
    local desc="$1" method="$2" url="$3" expected="$4" body="${5:-}"
    local status
    if [ "$method" = "POST" ]; then
        status=$(curl -s -o /dev/null -w "%{http_code}" -X POST "http://127.0.0.1:$PORT$url" -H 'Content-Type: application/json' -d "$body" 2>&1)
    else
        status=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT$url" 2>&1)
    fi
    if [ "$status" = "$expected" ]; then
        pass "$desc"
    else
        fail "$desc (expected HTTP $expected, got $status)"
    fi
}

# Assert on an already-fetched response (POST once, assert many times).
assert_contains() {   # desc response expected
    if grep -qF -- "$3" <<< "$2"; then pass "$1"; else fail "$1 (expected '$3', got: $2)"; fi
}
assert_not_contains() {   # desc response unexpected
    if grep -qF -- "$3" <<< "$2"; then fail "$1 (unexpected '$3' in: $2)"; else pass "$1"; fi
}
post_json() { acurl -X POST "http://127.0.0.1:$PORT$1" -H 'Content-Type: application/json' -d "$2" 2>&1; }

# Raw WebSocket upgrade request. On a 101 the server keeps the socket open, so
# curl hits --max-time and exits 28; `|| true` keeps `set -e` from aborting.
# Pass -o/-w/-b and the URL as extra args.
ws_upgrade() { curl -s --http1.1 --max-time 3 -H 'Connection: Upgrade' -H 'Upgrade: websocket' -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' "$@" || true; }

# Assert the HTTP status of a WebSocket upgrade. Extra curl args (e.g. -b jar)
# go after the URL.
assert_ws_status() {   # desc path expected [curl args...]
    local desc="$1" path="$2" expected="$3"
    shift 3
    local status
    status=$(ws_upgrade "$@" -o /dev/null -w '%{http_code}' "http://127.0.0.1:$PORT$path")
    if [ "$status" = "$expected" ]; then
        pass "$desc"
    else
        fail "$desc (expected HTTP $expected, got $status)"
    fi
}

# Build a /_presemble/block-edit request exactly as the browser does: find the
# editable element in the editorial page, take its emitted data-presemble-address
# and data-presemble-md, and call inject.js's buildBlockEditRequest (its
# `protocol-builders` region) on them.
#   block_edit_request <page-html> <file> <slot> <match> <old> <new> <intent>
# <match> picks the element (its markdown contains it); <new> replaces <old> in
# that markdown to give the proposed text. Prints the request JSON; exits
# non-zero (with a message on stderr) when there is no such editable element or
# inject.js has no builder.
# deno eval runs with all permissions implicitly, so no --allow-env is needed.
block_edit_request() {
    PAGE_HTML="$1" FILE="$2" SLOT="$3" MATCH="$4" OLD="$5" NEW="$6" INTENT="$7" deno eval '
const env = (k) => Deno.env.get(k) ?? "";
const src = Deno.readTextFileSync("resources/serve/inject.js");
const a = src.indexOf("// BEGIN protocol-builders");
const b = src.indexOf("// END protocol-builders");
if (a < 0 || b < a) { console.error("inject.js has no protocol-builders region"); Deno.exit(2); }
const { buildBlockEditRequest, editTarget } = new Function(
  src.slice(a, b) + "\nreturn { buildBlockEditRequest: buildBlockEditRequest, editTarget: editTarget };")();
const unesc = (s) => s.replace(/&quot;/g, "\"").replace(/&#0?39;|&#x27;/g, "\x27")
  .replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&amp;/g, "&");
const attr = (tag, name) => {
  const m = new RegExp("\\s" + name + "=\"([^\"]*)\"").exec(tag);
  return m ? unesc(m[1]) : null;
};
const tags = env("PAGE_HTML").match(/<[a-zA-Z][a-zA-Z0-9]*\s[^>]*data-presemble-address="[^"]*"[^>]*>/g) ?? [];
const hit = tags
  .map((t) => ({ file: attr(t, "data-presemble-file"), slot: attr(t, "data-presemble-slot"),
                 target: editTarget(attr(t, "data-presemble-address"), attr(t, "data-presemble-md")) }))
  .find((h) => h.file === env("FILE") && h.slot === env("SLOT") && h.target !== null && h.target.md.includes(env("MATCH")));
if (!hit) { console.error("no editable element in slot " + env("SLOT") + " containing " + env("MATCH")); Deno.exit(1); }
if (!hit.target.md.includes(env("OLD"))) { console.error("element markdown lacks " + env("OLD") + ": " + hit.target.md); Deno.exit(1); }
const proposed = hit.target.md.split(env("OLD")).join(env("NEW"));
console.log(JSON.stringify(buildBlockEditRequest(hit.file, hit.target.address, hit.target.md, proposed, env("INTENT"))));'
}

# Assert rep expression contains expected text
assert_rep() {
    local desc="$1" expr="$2" expected="$3"
    local response
    local port_dir="$(dirname "$SITE_DIR")"
    response=$(rep -p "@$port_dir/.nrepl-port" "$expr" 2>&1 || echo "REP_ERROR")
    if grep -qF "$expected" <<< "$response"; then
        pass "$desc"
    else
        fail "$desc (expected '$expected', got: $response)"
    fi
}

# ── Setup ──────────────────────────────────────────────────────────────────

log "Building presemble ($PROFILE profile)..."
cargo polylith cargo --profile "$PROFILE" build --bin presemble -q 2>/dev/null

# ── Test: Reproducible build ───────────────────────────────────────────────
# Builds site/ twice via the just recipes (site-snapshot, output-diff exact):
# the outputs must be byte-identical (e.g. collection item order).

log "Checking that building site/ twice gives identical output..."
snapshot() {   # prints the snapshot dir of a fresh `just site-snapshot site`
    just site-snapshot site 2>/dev/null | sed -n 's/^Snapshot: //p' | tail -n 1
}
REPRO_A=$(snapshot || true)
REPRO_B=$(snapshot || true)
if [ -z "$REPRO_A" ] || [ -z "$REPRO_B" ]; then
    fail "reproducible build: just site-snapshot site failed"
elif REPRO_DIFF=$(just output-diff "$REPRO_A" "$REPRO_B" exact 2>&1); then
    pass "reproducible build: two builds of site/ are identical"
else
    fail "reproducible build: two builds of site/ differ"
    head -n 40 <<< "$REPRO_DIFF"
fi

SITE_DIR=$(mktemp -d /tmp/presemble-smoke-XXXXXX)
# Cookie jar lives outside $SITE_DIR so the file watcher never sees it.
COOKIE_JAR=$(mktemp /tmp/presemble-smoke-cookies-XXXXXX)
log "Site directory: $SITE_DIR"

# Seed legacy (pre-ADR-056) NED suggestions BEFORE serve starts — the
# conductor loads suggestions only at startup. Does not affect the empty-site
# welcome page: is_source_empty only inspects schemas/ and content/.
mkdir -p "$SITE_DIR/.presemble/suggestions/ned"
# Auto-save off (ADR-070): the sections below check unsaved edits, conflicts
# and explicit saves, which a save after 2 s idle would race. Auto-save itself
# is covered end to end by editor_server's the_ticker_saves_an_idle_document_end_to_end.
cat > "$SITE_DIR/.presemble/config.json" <<'JSON'
{"autosave": {"enabled": false}}
JSON
cat > "$SITE_DIR/.presemble/suggestions/ned/sug-legacy-smoke.json" <<'JSON'
{"id":"sug-legacy-smoke","author":{"Human":"smoketest"},"file":"content/post/hello-world.md","selection":"(ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")","mutation":{"SetText":"Legacy Title"},"workspace_hash":"untracked","reason":"legacy slot-addressed","status":"Pending","created_at":"2026-01-01T00:00:00Z"}
JSON
cat > "$SITE_DIR/.presemble/suggestions/ned/sug-legacy-stale.json" <<'JSON'
{"id":"sug-legacy-stale","author":{"Human":"smoketest"},"file":"content/post/hello-world.md","selection":"(ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")","mutation":{"SetText":"x"},"workspace_hash":"untracked","reason":"legacy stale no cause","status":{"Stale":{"reason":"old"}},"created_at":"2026-01-01T00:00:00Z"}
JSON

# ── Test: invalid PRESEMBLE_SESSION_KEY refuses to start (ADR-055 amendment) ──
log "Checking that an empty/short PRESEMBLE_SESSION_KEY fails fast..."
KEY_CHECK_DIR=$(mktemp -d /tmp/presemble-keycheck-XXXXXX)
check_bad_key() {  # $1 = key value, $2 = expected message fragment, $3 = label
    local out rc=0
    out=$(PRESEMBLE_SESSION_KEY="$1" timeout 300 cargo polylith cargo --profile "$PROFILE" run --quiet --bin presemble -- serve "$KEY_CHECK_DIR/" --port 3999 2>&1) || rc=$?
    if [ "$rc" -eq 0 ] || [ "$rc" -eq 124 ]; then fail "$3: serve did not exit with an error (rc=$rc)"; return; fi
    if ! grep -qF "$2" <<<"$out"; then fail "$3: missing '$2' in output"; return; fi
    if grep -qF "MagicLinkSessionStore" <<<"$out"; then fail "$3: serve selected Hosted mode"; return; fi
    if [ -n "$1" ] && grep -qF "$1" <<<"$out"; then fail "$3: error output leaked the key"; return; fi
    pass "$3"
}
check_bad_key ""                       "PRESEMBLE_SESSION_KEY is set but empty" "empty session key refuses to start"
check_bad_key "short-but-secret-value" "PRESEMBLE_SESSION_KEY is too short"     "short session key refuses to start"
rm -rf "$KEY_CHECK_DIR"

# Start serve with a dev session (ADR-054/055: editorial routes need a session).
# PRESEMBLE_SESSION_KEY would make serve ignore --dev-session, so unset it.
log "Starting presemble serve..."
# Clean up stale nrepl-port files
rm -f "$(dirname "$SITE_DIR")/.nrepl-port"
# `env` execs cargo, so $! is still the cargo process, as before.
env -u PRESEMBLE_SESSION_KEY cargo polylith cargo --profile "$PROFILE" run --bin presemble -- serve "$SITE_DIR/" --dev-session=smoketest > "$SITE_DIR/serve.log" 2>&1 &
PID=$!

# Wait for server to be ready
for i in $(seq 1 30); do
    if curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

if ! curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
    echo -e "${RED}Server failed to start${NC}"
    cat "$SITE_DIR/serve.log"
    exit 1
fi

log "Server ready on port $PORT"

# ── Login (dev session) ───────────────────────────────────────────────────
# serve prints "Login URL: http://127.0.0.1:<port>/_presemble/login?token=..."
# using its bind port, which is the default 3000 == $PORT here.

login_fail() { echo -e "${RED}Login failed: $1${NC}"; cat "$SITE_DIR/serve.log"; exit 1; }
LOGIN_URL=""
for i in $(seq 1 20); do
    LOGIN_URL=$(sed -n '/Login URL:/{s/.*Login URL:[[:space:]]*//p;q}' "$SITE_DIR/serve.log")
    [ -n "$LOGIN_URL" ] && break
    sleep 0.25
done
[ -n "$LOGIN_URL" ] || login_fail "no 'Login URL:' line in serve.log (is --dev-session wired?)"
LOGIN_STATUS=$(curl -s -o /dev/null -w '%{http_code}' -c "$COOKIE_JAR" "$LOGIN_URL")
[ "$LOGIN_STATUS" = "302" ] || login_fail "GET /_presemble/login returned $LOGIN_STATUS (expected 302)"
grep -qF presemble_session "$COOKIE_JAR" || login_fail "no presemble_session cookie in jar"
PROBE=$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/_presemble/session")
[ "$PROBE" = "200" ] || login_fail "authenticated probe returned $PROBE"
log "Logged in (dev session)"

# ── Test: Auth boundary (ADR-054) ──────────────────────────────────────────
# Plain curl, no cookie.

log "Testing auth boundary..."
BLOCK_EDIT_PROBE='{"file":"content/index.md","selection":"(ned/slot (ned/doc-by-path \"content/index.md\") \"tagline\")","base":{"markdown":""},"proposed":{"markdown":"x"},"intent":"apply"}'
assert_status_anon "editorial POST without session is 401" POST "/_presemble/block-edit" "401" \
    "$BLOCK_EDIT_PROBE"
ANON_BODY=$(curl -s -X POST "http://127.0.0.1:$PORT/_presemble/block-edit" -H 'Content-Type: application/json' \
    -d "$BLOCK_EDIT_PROBE" 2>&1 || true)
assert_contains "anonymous 401 explains not logged in" "$ANON_BODY" "Not logged in"
assert_not_contains "anonymous 401 (dev session on) does not suggest --dev-session" "$ANON_BODY" "--dev-session"
assert_status_anon "schema URL without session is 401" GET "/_schema/index" "401"
assert_status_anon "login with bogus token is 401" GET "/_presemble/login?token=bogus.token" "401"
assert_status_anon "published page without session is 200" GET "/" "200"
assert_status_anon "session probe without session is 401" GET "/_presemble/session" "401"

# WebSocket split (ADR-054): public live-reload socket vs session-gated
# editorial socket. The 101 assertions rely on curl's -w '%{http_code}'
# reporting 101 even though the transfer then times out (exit 28) — if that
# does not hold for the curl in devenv, fall back to ws-smoke.ts coverage.
assert_ws_status "anonymous editorial-ws upgrade is 401" "/_presemble/editorial-ws" "401"
WS_ANON_BODY=$(ws_upgrade "http://127.0.0.1:$PORT/_presemble/editorial-ws")
assert_contains "anonymous editorial-ws 401 explains not logged in" "$WS_ANON_BODY" "Not logged in"
assert_ws_status "anonymous public ws upgrade is 101" "/_presemble/ws" "101"   # 101-on-timeout assumption
assert_ws_status "editorial-ws upgrade with session is 101" "/_presemble/editorial-ws" "101" -b "$COOKIE_JAR"   # 101-on-timeout assumption

# ── Test: Welcome page ─────────────────────────────────────────────────────

log "Testing welcome page..."
assert_curl "welcome page on empty site" "/" GET "" "Welcome to Presemble"

# ── Test: Scaffold blog ────────────────────────────────────────────────────

log "Scaffolding blog site..."
assert_curl "scaffold blog" "/_presemble/scaffold" POST '{"template":"blog","format":"hiccup"}' '"ok":true'

# ── Test: Stylesheet served after scaffold ─────────────────────────────────

log "Testing stylesheet..."
assert_status "stylesheet served after scaffold" "/assets/style.css" "200"

# ── Test: Schemas endpoint ─────────────────────────────────────────────────

log "Testing schemas..."
assert_curl "schemas list includes post" "/_presemble/schemas" GET "" "post"
assert_curl "schemas list includes author" "/_presemble/schemas" GET "" "author"

# ── Test: Link picker returns schema-correct options ──────────────────────

log "Testing link picker..."
assert_curl "link picker for author slot returns author options" \
    "/_presemble/links?schema=post&slot=author" GET "" \
    '"href":"/author/default"'
assert_curl_not_contains "link picker for author slot excludes posts" \
    "/_presemble/links?schema=post&slot=author" \
    "/post/"

# ── Test: Index page built ─────────────────────────────────────────────────

log "Testing index page..."
assert_curl "index page exists" "/" GET "" "html"
assert_curl "index page has data-presemble-file attr" "/" GET "" 'data-presemble-file="content/index.md"'
assert_curl "collection page has index data-presemble-file" "/post/" GET "" 'data-presemble-file="content/post/index.md"'
assert_curl "collection page has item data-presemble-file" "/post/" GET "" 'data-presemble-file="content/post/hello-world.md"'
assert_curl "leaf page has data-presemble-file attr" "/post/hello-world" GET "" 'data-presemble-file="content/post/hello-world.md"'
# The browser edits the canonical markdown and addresses by the emitted
# address (ADR-059 section 10); the old stored-text attribute is gone.
assert_curl "post title element carries its markdown" "/post/hello-world" GET "" \
    'data-presemble-slot="title" data-presemble-file="content/post/hello-world.md" data-presemble-md="Hello, World: Your First Post"'
assert_curl "post title element carries its address" "/post/hello-world" GET "" 'data-presemble-address='
assert_curl_not_contains "editorial page carries no data-presemble-text" "/post/hello-world" 'data-presemble-text'

# ── Test: Reader vs editor injection (ADR-054) ────────────────────────────
# Lives here, not in the auth-boundary section: /post/hello-world only exists
# after the scaffold.

log "Testing reader vs editor script injection..."
# Readers are served the published output (saved and valid), never the working
# copy. The scaffolded site has not been published yet, so a reader gets a 404
# with no page and no editor code; the reader script is asserted once the site
# is published (after the serve restart at the end).
READER_PAGE=$(curl -s "http://127.0.0.1:$PORT/post/hello-world" 2>&1 || true)
assert_status_anon "a page that was never published is a 404 for readers" GET "/post/hello-world" "404"
assert_not_contains "anonymous page has no editorial socket" "$READER_PAGE" "editorial-ws"
EDITOR_PAGE=$(acurl "http://127.0.0.1:$PORT/post/hello-world" 2>&1 || true)
assert_contains "logged-in page gets editorial socket" "$EDITOR_PAGE" "editorial-ws"

# ── Test: Edit index tagline ───────────────────────────────────────────────

log "Editing index tagline..."
block_edit "edit index tagline" "/" content/index.md tagline "Smoke Test Blog"

# ── Test: Dirty buffers ───────────────────────────────────────────────────

log "Testing dirty buffers..."
assert_curl "dirty buffers after edit" "/_presemble/dirty-buffers" GET "" "content/index.md"

# ── Test: Save all ─────────────────────────────────────────────────────────

log "Saving all buffers..."
assert_curl "save all" "/_presemble/save-all" POST "" '"ok":true'
assert_curl "no dirty buffers after save" "/_presemble/dirty-buffers" GET "" "[]"

# ── Test: nth-child scopes edit to one paragraph, leaves others intact ────

log "Testing nth-child slot edit scoping..."
# A raw /apply program addressing one text exactly: (ned/text-in ...) over the
# second child of the summary slot.
NED_PROGRAM='(ned/set-text (ned/text-in (ned/nth-child (ned/slot (ned/doc-by-path "content/post/hello-world.md") "summary") 1)) "NTHCHILD ONLY SECOND")'
# Build JSON by escaping the double quotes inside the program string
APPLY_BODY='{"program":"'"$(echo "$NED_PROGRAM" | sed 's/"/\\"/g')"'"}'
assert_curl "apply nth-child edit to 2nd paragraph" "/_presemble/apply" POST "$APPLY_BODY" '"dirtyPaths":1'
assert_curl "nth-child edit: 2nd paragraph updated" "/post/hello-world" GET "" "NTHCHILD ONLY SECOND"
assert_curl "nth-child edit: 1st paragraph unchanged" "/post/hello-world" GET "" "Welcome to your new blog"

# A program that addresses several nodes is refused as a whole, and the refusal
# is structured: the step, the builtin and the cause, next to the message.
NED_AMBIGUOUS='(ned/set-text (-> (ned/slot (ned/doc-by-path "content/post/hello-world.md") "summary") ned/descendants ned/texts) "SHOULD NOT APPLY")'
AMBIGUOUS_BODY='{"program":"'"$(echo "$NED_AMBIGUOUS" | sed 's/"/\\"/g')"'"}'
assert_curl "ambiguous apply is refused" "/_presemble/apply" POST "$AMBIGUOUS_BODY" '"ok":false'
assert_curl "ambiguous apply names the step" "/_presemble/apply" POST "$AMBIGUOUS_BODY" '"step":1'
assert_curl "ambiguous apply names the builtin" "/_presemble/apply" POST "$AMBIGUOUS_BODY" '"origin":"ned/set-text"'
assert_curl "ambiguous apply carries the cause" "/_presemble/apply" POST "$AMBIGUOUS_BODY" '"cause"'
assert_curl "ambiguous apply changed nothing" "/post/hello-world" GET "" "NTHCHILD ONLY SECOND"

# ── Test: presemble reload throws away one document's unsaved edits ───────

log "Testing presemble reload..."
assert_curl "hello-world is dirty before the reload" "/_presemble/dirty-buffers" GET "" "content/post/hello-world.md"
RELOAD_OUT=$(cargo polylith cargo --profile "$PROFILE" run --quiet --bin presemble -- reload "$SITE_DIR/" content/post/hello-world.md 2>&1)
assert_contains "reload says it discarded unsaved edits" "$RELOAD_OUT" "discarded unsaved edits"
assert_curl "reload shows the disk text" "/post/hello-world" GET "" "Presemble keeps your content well-structured"
assert_curl_not_contains "reload dropped the edit" "/post/hello-world" "NTHCHILD ONLY SECOND"
assert_curl_not_contains "reload clears the dirty buffer" "/_presemble/dirty-buffers" "content/post/hello-world.md"

# ── Test: a disk change to a dirty document is a conflict (ADR-065) ───────
# Unsaved edits are never discarded automatically. Saving over a conflict needs
# consent that names the file; `presemble reload` takes the disk version.

log "Testing disk conflicts..."
HW_FILE="content/post/hello-world.md"
HW_PATH="$SITE_DIR/$HW_FILE"
HW_ORIG=$(cat "$HW_PATH")
# Poll until a GET of $1 contains $2 (up to ~10 s): the watcher is asynchronous.
wait_for_page_text() {
    local url="$1" text="$2" i
    for i in $(seq 1 50); do
        # Capture, then grep a herestring: see assert_curl (SIGPIPE under pipefail).
        local page
        page=$(acurl "http://127.0.0.1:$PORT$url" 2>/dev/null || true)
        if grep -qF -- "$text" <<< "$page"; then return 0; fi
        sleep 0.2
    done
    return 1
}
conflict_edit() {   # set the title through /apply; $1 is the new title
    local program body
    program='(ned/set-text (ned/text-in (ned/children (ned/slot (ned/doc-by-path "'"$HW_FILE"'") "title"))) "'"$1"'")'
    body='{"program":"'"$(echo "$program" | sed 's/"/\\"/g')"'"}'
    post_json "/_presemble/apply" "$body"
}

# 1-2. Edit in the browser, then overwrite the file from the shell.
assert_contains "conflict scenario: edit applied" "$(conflict_edit 'Edited In The Browser')" '"dirtyPaths":1'
sed 's/Hello, World: Your First Post/Shell Wrote This Title/' "$HW_PATH" > "$HW_PATH.tmp" && mv "$HW_PATH.tmp" "$HW_PATH"
wait_for_page_text "/_presemble/conflicts" "$HW_FILE" || true

# 3. The edit is kept and the conflict is listed.
assert_curl "disk change to a dirty file is listed as a conflict" "/_presemble/conflicts" GET "" "$HW_FILE"
assert_curl "conflict keeps the unsaved edit on the page" "/post/hello-world" GET "" "Edited In The Browser"

# 4. Save-all with no consent warns and writes nothing.
REFUSED=$(post_json "/_presemble/save-all" "")
assert_contains "save-all needs confirmation for the conflict" "$REFUSED" '"needsConfirmation":true'
assert_contains "save-all names the conflicted file" "$REFUSED" "$HW_FILE"
if grep -qF "Shell Wrote This Title" "$HW_PATH"; then pass "save-all with a conflict wrote nothing"; else fail "save-all with a conflict wrote nothing (disk: $(head -c 200 "$HW_PATH"))"; fi

# 5. Consent that names the file overwrites it.
SAVED=$(post_json "/_presemble/save-all" '{"overwriteConflicts":["'"$HW_FILE"'"]}')
assert_contains "save-all with consent is ok" "$SAVED" '"ok":true'
if grep -qF "Edited In The Browser" "$HW_PATH"; then pass "save-all with consent wrote the edit"; else fail "save-all with consent wrote the edit (disk: $(head -c 200 "$HW_PATH"))"; fi
assert_curl "the conflict is cleared after saving" "/_presemble/conflicts" GET "" "[]"

# 6. Edit again, change the file from outside again, then take the disk version.
conflict_edit 'Edited Again' > /dev/null
sed 's/Edited In The Browser/Shell Again Title/' "$HW_PATH" > "$HW_PATH.tmp" && mv "$HW_PATH.tmp" "$HW_PATH"
wait_for_page_text "/_presemble/conflicts" "$HW_FILE" || true
assert_curl "a second disk change is a conflict again" "/_presemble/conflicts" GET "" "$HW_FILE"
CONFLICT_RELOAD=$(cargo polylith cargo --profile "$PROFILE" run --quiet --bin presemble -- reload "$SITE_DIR/" "$HW_FILE" 2>&1)
assert_contains "reload discards the conflicted edits" "$CONFLICT_RELOAD" "discarded unsaved edits"
assert_curl "reload shows the disk text of the conflicted file" "/post/hello-world" GET "" "Shell Again Title"
assert_curl "reload resolves the conflict" "/_presemble/conflicts" GET "" "[]"

# Restore the scaffolded file for the sections below.
printf '%s\n' "$HW_ORIG" > "$HW_PATH"
wait_for_page_text "/post/hello-world" "Hello, World: Your First Post" || true
assert_curl "hello-world is back to its scaffolded text" "/post/hello-world" GET "" "Hello, World: Your First Post"

# 7. A conflict on a block is marked on that block in the editorial page, and
#    resolved there (ADR-065 amendment F2).
conflict_id() {   # the first conflict id the page at $1 marks a block with
    acurl "http://127.0.0.1:$PORT$1" 2>/dev/null \
        | grep -oE 'data-presemble-conflict="[^"]*' \
        | grep -oE '&quot;id&quot;:&quot;[0-9a-f]{16}' \
        | grep -oE '[0-9a-f]{16}$' | head -1 || true
}
resolve_body() {   # $1 the conflict id, $2 disk|unsaved
    printf '{"file":"%s","id":"%s","take":"%s"}' "$HW_FILE" "$1" "$2"
}
# The attribute as written on an element (inject.js names it too).
MARKED='data-presemble-conflict="'
conflict_edit 'Edited For Blocks' > /dev/null
sed 's/Hello, World: Your First Post/Shell Block Title/' "$HW_PATH" > "$HW_PATH.tmp" && mv "$HW_PATH.tmp" "$HW_PATH"
wait_for_page_text "/post/hello-world" "$MARKED" || true
assert_curl "a conflicted block is marked on the page" "/post/hello-world" GET "" "$MARKED"
assert_curl "the marker carries the file's version" "/post/hello-world" GET "" "Shell Block Title"
assert_curl "the page keeps the unsaved edit of the block" "/post/hello-world" GET "" "Edited For Blocks"
BLOCK_ID=$(conflict_id "/post/hello-world")
assert_contains "keeping the unsaved version resolves the block" "$(post_json "/_presemble/resolve-conflict" "$(resolve_body "$BLOCK_ID" unsaved)")" '"ok":true'
assert_curl "the resolved conflict is no longer listed" "/_presemble/conflicts" GET "" "[]"
assert_curl_not_contains "the page no longer marks the block" "/post/hello-world" "$MARKED"
assert_contains "an old conflict id is refused" "$(post_json "/_presemble/resolve-conflict" "$(resolve_body "$BLOCK_ID" disk)")" '"ok":false'

sed 's/Shell Block Title/Shell Block Again/' "$HW_PATH" > "$HW_PATH.tmp" && mv "$HW_PATH.tmp" "$HW_PATH"
wait_for_page_text "/post/hello-world" "$MARKED" || true
assert_curl "a second change of the block is marked again" "/post/hello-world" GET "" "$MARKED"
BLOCK_ID=$(conflict_id "/post/hello-world")
assert_contains "taking the file's version resolves the block" "$(post_json "/_presemble/resolve-conflict" "$(resolve_body "$BLOCK_ID" disk)")" '"ok":true'
assert_curl "the page shows the file's version of the block" "/post/hello-world" GET "" "Shell Block Again"
assert_curl_not_contains "the taken block is no longer marked" "/post/hello-world" "$MARKED"
assert_curl "nothing conflicts any more" "/_presemble/conflicts" GET "" "[]"

# Restore the scaffolded file (merged into N: only the file changed) and save.
printf '%s\n' "$HW_ORIG" > "$HW_PATH"
wait_for_page_text "/post/hello-world" "Hello, World: Your First Post" || true
post_json "/_presemble/save-all" "" > /dev/null
assert_curl "hello-world is back to its scaffolded text again" "/post/hello-world" GET "" "Hello, World: Your First Post"
assert_curl_not_contains "hello-world has no unsaved edits" "/_presemble/dirty-buffers" "$HW_FILE"

# ── Test: Diagnostics in the editorial page (FU-7b) ───────────────────────
# A copy of hello-world with inline HTML in a body paragraph: the editorial page
# carries the diagnostics (element attribute + page script). The probe is fixed
# afterwards and stays in the site: the closed section at the end asserts that
# its published output carries neither (a site with an error is never published).

# inject.js is inlined in editorial pages and names the attribute (without the
# `="`), so the attribute assertions include the opening quote of its value.
log "Testing editorial diagnostics..."
PROBE_FILE="content/post/diag-probe.md"
PROBE_PATH="$SITE_DIR/$PROBE_FILE"
{ cat "$HW_PATH"; printf '\nProbe paragraph with <b>x</b> inline html.\n'; } > "$PROBE_PATH"
if wait_for_page_text "/post/diag-probe" "Probe paragraph"; then
    pass "diagnostics: the probe page is built"
else
    fail "diagnostics: the probe page is built (no 'Probe paragraph' on /post/diag-probe after ~10 s)"
fi
assert_curl "diagnostics: editorial page carries the element attribute" "/post/diag-probe" GET "" 'data-presemble-diagnostics="'
assert_curl "diagnostics: editorial page carries the page script" "/post/diag-probe" GET "" 'id="presemble-diagnostics"'
assert_curl "diagnostics: page script counts the error" "/post/diag-probe" GET "" '"errors":1'
assert_curl "diagnostics: a valid page counts no errors" "/post/hello-world" GET "" '"errors":0'
assert_curl_not_contains "diagnostics: a valid page has no element diagnostics" "/post/hello-world" 'data-presemble-diagnostics="'
sed 's|<b>x</b>|x|' "$PROBE_PATH" > "$PROBE_PATH.tmp" && mv "$PROBE_PATH.tmp" "$PROBE_PATH"
DIAG_POLLS=0
for i in $(seq 1 50); do
    DIAG_POLLS=$i
    # Capture, then grep a herestring: piped into `grep -q` under pipefail, an
    # early match SIGPIPEs curl and reads as "no match", ending the wait at once.
    DIAG_PAGE=$(acurl "http://127.0.0.1:$PORT/post/diag-probe" 2>/dev/null || true)
    if ! grep -qF '"errors":1' <<< "$DIAG_PAGE"; then break; fi
    sleep 0.2
done
DIAG_FAIL_BEFORE=$FAIL
assert_curl_not_contains "diagnostics: fixing the inline html clears the attribute" "/post/diag-probe" 'data-presemble-diagnostics="'
assert_curl "diagnostics: fixing the inline html zeroes the page script" "/post/diag-probe" GET "" '"errors":0'
# Keep the evidence of a failure before cleanup removes the site: how long the
# wait above polled, and the end of the serve log (its `rebuild:` lines).
if [ "$FAIL" -gt "$DIAG_FAIL_BEFORE" ]; then
    echo "  diagnostics: the wait for the fix stopped after $DIAG_POLLS poll(s); serve.log (last 50 lines):"
    tail -n 50 "$SITE_DIR/serve.log" | sed 's/^/    | /'
fi

# ── Test: Suggestions ─────────────────────────────────────────────────────

log "Testing suggestions..."

# ── Test: Create suggestion ───────────────────────────────────────────────

log "Creating suggestion via MCP-style endpoint..."
# (suggestions are created via the conductor, not HTTP — skip for now unless MCP is connected)

# ── Test: NED suggestions via HTTP ────────────────────────────────────────

log "Testing NED suggestions via HTTP..."

NED_SUG_BODY='{"file":"content/post/hello-world.md","selection":"(ned/text-in (ned/children (ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")))","mutation":{"SetText":"Suggested Title"},"reason":"smoketest"}'
NED_SUG_RESPONSE=$(acurl -X POST "http://127.0.0.1:$PORT/_presemble/ned-suggestions" -H 'Content-Type: application/json' -d "$NED_SUG_BODY" 2>&1)

assert_contains "ned_suggestions_create_returns_ok" "$NED_SUG_RESPONSE" '"ok":true'
assert_contains "ned_suggestions_create_returns_id" "$NED_SUG_RESPONSE" '"id":"sug-'

# Capture the suggestion id for subsequent reject
NED_SUG_ID=$(grep -o '"id":"[^"]*"' <<< "$NED_SUG_RESPONSE" | head -1 | sed 's/"id":"//;s/"//' || true)

assert_curl "ned_suggestions_list_for_file_reason" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"reason":"smoketest"'
# The seeded legacy suggestions also produce a slot anchor, so assert on this
# suggestion's exact (ned/text-in ...) selection instead of the anchor kind.
assert_curl "ned_suggestions_list_for_file_exact_selection" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"selection":"(ned/text-in'

# NOTE: the seeded legacy suggestions also target this file, so this only
# proves the endpoint lists it — creation is proven by the checks above.
assert_curl "ned_suggestion_files_includes_file" "/_presemble/ned-suggestion-files" GET "" '"content/post/hello-world.md"'

NED_REJECT_BODY='{"id":"'"$NED_SUG_ID"'"}'
assert_curl "ned_suggestions_reject_marks_rejected" "/_presemble/ned-suggestions/reject" POST "$NED_REJECT_BODY" '"ok":true'

assert_curl "ned_suggestions_list_after_reject_contains_rejected" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"Rejected"'

NED_BAD_BODY='{"file":"content/post/hello-world.md","selection":"(ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")","mutation":{"Replace":[{"Existing":0}]},"reason":"reject me"}'
R=$(post_json /_presemble/ned-suggestions "$NED_BAD_BODY")
assert_contains "ned_suggestions_create_rejects_existing_node_ok_false" "$R" '"ok":false'
assert_contains "ned_suggestions_create_rejects_existing_node_mentions_existing" "$R" 'Existing'

# ── Test: Exact addressing (ADR-056) ──────────────────────────────────────
# Order matters: D rewrites the "What to Edit" paragraph that C's ambiguity
# depends on, and E runs after D.

# A. Accept an exactly-addressed SetText on the title
log "Testing exact-addressed title suggestion accept..."
ACC_BODY='{"file":"content/post/hello-world.md","selection":"(ned/text-in (ned/children (ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")))","mutation":{"SetText":"Accepted Smoke Title"},"reason":"smoketest accept"}'
R=$(post_json /_presemble/ned-suggestions "$ACC_BODY")
assert_contains "exact title suggestion created" "$R" '"ok":true'
ACC_ID=$(grep -o '"id":"[^"]*"' <<< "$R" | head -1 | sed 's/"id":"//;s/"//' || true)
R=$(post_json /_presemble/ned-suggestions/accept '{"id":"'"$ACC_ID"'"}')
assert_contains "accept exact title suggestion" "$R" '"ok":true'

PAGE=$(acurl "http://127.0.0.1:$PORT/post/hello-world")
count=$(grep -oF '>Accepted Smoke Title<' <<< "$PAGE" | wc -l || true)
if [ "$((count))" -eq 1 ]; then
    pass "accepted title rendered exactly once"
else
    fail "accepted title rendered exactly once (found $((count)) occurrences of '>Accepted Smoke Title<')"
fi
assert_curl_not_contains "old title gone" "/post/hello-world" "Hello, World: Your First Post"
assert_curl "title accept marks doc dirty" "/_presemble/dirty-buffers" GET "" "content/post/hello-world.md"
assert_curl "accepted suggestion listed as Accepted" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"Accepted"'

# B. A slot-addressed SetText is rejected with a hint
log "Testing slot-addressed SetText rejection..."
R=$(post_json /_presemble/ned-suggestions '{"file":"content/post/hello-world.md","selection":"(ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")","mutation":{"SetText":"Should Not Apply"},"reason":"slot address"}')
assert_contains "slot-addressed SetText rejected" "$R" '"ok":false'
assert_contains "slot-addressed SetText hint mentions ned/text-in" "$R" 'ned/text-in'

# C. An ambiguous body SearchReplace is rejected
# ("file" occurs in both body paragraphs of the scaffolded hello-world.md.)
log "Testing ambiguous SearchReplace rejection..."
R=$(post_json /_presemble/ned-suggestions '{"file":"content/post/hello-world.md","selection":"(ned/text-containing (ned/body (ned/doc-by-path \"content/post/hello-world.md\")) \"file\")","mutation":{"SearchReplace":{"search":"file","replace":"document"}},"reason":"ambiguous"}')
assert_contains "ambiguous SearchReplace rejected" "$R" '"ok":false'
assert_contains "ambiguous SearchReplace says must match exactly one" "$R" 'must match exactly one'

# C2. A mutating selection is rejected and the doc is left intact
log "Testing mutating-selection rejection..."
R=$(post_json /_presemble/ned-suggestions '{"file":"content/post/hello-world.md","selection":"(do (ned/delete (ned/doc-by-path \"content/post/hello-world.md\")) (ned/doc-by-path \"content/post/hello-world.md\"))","mutation":"Delete","reason":"mutating selection"}')
assert_contains "mutating selection rejected" "$R" '"ok":false'
assert_contains "mutating selection could not be evaluated" "$R" 'could not be evaluated'
assert_curl "doc intact after mutating selection" "/_presemble/render?path=/post/hello-world&mode=view" GET "" "Getting Started"

# D. The browser's four editors through POST /_presemble/block-edit
# (ADR-059 section 10). Each request is built from the editorial page's emitted
# data-presemble-address / data-presemble-md by inject.js's own builder
# (block_edit_request above), so these exercise the browser's wire format.
log "Testing block-edit requests built by inject.js..."
BE_FILE="content/post/hello-world.md"
# POST the block-edit for <slot> <match> <old> <new> <intent>; prints the response.
be_post() {
    local page req
    page=$(acurl "http://127.0.0.1:$PORT/post/hello-world")
    req=$(block_edit_request "$page" "$BE_FILE" "$1" "$2" "$3" "$4" "$5" 2>&1) || {
        echo "BUILD_FAILED: $req"
        return 0
    }
    post_json /_presemble/block-edit "$req"
}

# D1. Body edit (apply): the paragraph under "What to Edit".
R=$(be_post body "Replace this file with your own story" \
    "Replace this file with your own story. Update the title, summary, author link, and body to match your content." \
    "Smoke body edit via bsave." apply)
assert_contains "body edit (apply) is applied" "$R" '"applied"'
assert_contains "body edit (apply) rebuilds the page" "$R" '/post/hello-world'
assert_curl "body edit rendered" "/post/hello-world" GET "" "Smoke body edit via bsave."
assert_curl "sibling heading intact" "/post/hello-world" GET "" "What to Edit"
assert_curl "other body para intact" "/post/hello-world" GET "" "Open the"
assert_curl "block-edit marks dirty" "/_presemble/dirty-buffers" GET "" "content/post/hello-world.md"

# D2. Slot edit (apply): the title heading.
R=$(be_post title "Accepted Smoke Title" "Accepted Smoke Title" "Accepted Smoke Title, slot-edited" apply)
assert_contains "slot edit (apply) is applied" "$R" '"applied"'
assert_curl "slot edit rendered" "/post/hello-world" GET "" "Accepted Smoke Title, slot-edited"

# D3. Body suggest: stored, not applied.
R=$(be_post body "Open the" "Open the" "Open up the" suggest)
assert_contains "body suggest creates a suggestion" "$R" '"suggestion-created"'
assert_contains "body suggest returns an id" "$R" '"id":"sug-'
assert_curl_not_contains "body suggest does not change the page" "/post/hello-world" "Open up the"

# D4. Slot suggest: stored, not applied.
R=$(be_post title "slot-edited" "slot-edited" "slot-suggested" suggest)
assert_contains "slot suggest creates a suggestion" "$R" '"suggestion-created"'
assert_curl_not_contains "slot suggest does not change the page" "/post/hello-world" "slot-suggested"

# D5. Structural suggest: one paragraph into two lowers to a Replace
# suggestion whose preview (base and proposed markdown) the review UI diffs.
R=$(be_post body "Open the" ". Each file follows" ".

Each file follows" suggest)
assert_contains "structural suggest creates a suggestion" "$R" '"suggestion-created"'
LIST=$(acurl "http://127.0.0.1:$PORT/_presemble/ned-suggestions?file=$BE_FILE")
assert_contains "structural suggest is stored as a Replace" "$LIST" '"Replace"'
assert_contains "structural suggest carries its preview (base)" "$LIST" '"preview":{"base":{"markdown":"Open the'
assert_contains "structural suggest preview holds the proposed markdown" "$LIST" '"proposed":{"markdown":"Open the'

# D6. A refusal is a 200 with a typed message, and changes nothing.
R=$(be_post body "Smoke body edit via bsave" "Smoke body edit via bsave." "" apply)
assert_contains "block-edit refuses an empty block" "$R" '"refused"'
assert_curl "refused edit left the paragraph" "/post/hello-world" GET "" "Smoke body edit via bsave."

assert_curl "save after block-edit" "/_presemble/save-all" POST "" '"ok":true'

F="$SITE_DIR/content/post/hello-world.md"
if grep -qF 'Smoke body edit via bsave.' "$F"; then
    pass "block-edit edit survives save"
else
    fail "block-edit edit survives save (not found in $F)"
fi
if grep -qF '# Accepted Smoke Title' "$F"; then
    pass "accepted title survives save"
else
    fail "accepted title survives save (not found in $F)"
fi
if [ "$(grep -c '^## What to Edit$' "$F" || true)" -eq 1 ]; then
    pass "heading not duplicated on save"
else
    fail "heading not duplicated on save ($(grep -c '^## What to Edit$' "$F" || true) occurrences in $F)"
fi
assert_curl "no dirty buffers after block-edit save" "/_presemble/dirty-buffers" GET "" "[]"

# E. Legacy (pre-ADR-056) suggestions seeded before serve
log "Testing legacy suggestions..."
LIST=$(acurl "http://127.0.0.1:$PORT/_presemble/ned-suggestions?file=content/post/hello-world.md")
assert_contains "legacy stale without cause deserialises as Unspecified" "$LIST" '"cause":"Unspecified"'
# Going stale on accept still returns ok:true.
R=$(post_json /_presemble/ned-suggestions/accept '{"id":"sug-legacy-smoke"}')
assert_contains "legacy slot-addressed accept returns ok" "$R" '"ok":true'
LIST=$(acurl "http://127.0.0.1:$PORT/_presemble/ned-suggestions?file=content/post/hello-world.md")
assert_contains "legacy accept goes stale with WrongKind" "$LIST" '"WrongKind"'
assert_contains "legacy accept WrongKind expects Text" "$LIST" '"expected":"Text"'
assert_curl "legacy accept did not change title" "/post/hello-world" GET "" "Accepted Smoke Title"
assert_curl_not_contains "legacy title not applied" "/post/hello-world" "Legacy Title"

# ── Test: WebSocket broadcasts (tools/ws-smoke.ts) ────────────────────────
# Runs after E: ws-smoke.ts targets the "Smoke body edit via bsave." paragraph
# written in D, and finally replaces body element 3 of hello-world.md with a
# block-edit apply built from the editorial page by inject.js's own
# buildBlockEditRequest. It opens the public /_presemble/ws anonymously and
# /_presemble/editorial-ws with the session cookie, and checks no editorial
# message leaks onto the public socket. It prints PASS/FAIL lines and a final
# DONE on stdout.

log "Testing WebSocket broadcasts..."
# curl writes HttpOnly cookies as "#HttpOnly_<domain>", so '#' lines must NOT
# be filtered out here.
C=$(awk '$6=="presemble_session"{print $7}' "$COOKIE_JAR")
WS_ERR=$(mktemp /tmp/presemble-smoke-wserr-XXXXXX)
WS_RC=0
# The conflict step rewrites hello-world.md from outside, like an editor would.
WS_OUT=$(PRESEMBLE_SMOKE_BASE="http://127.0.0.1:$PORT" PRESEMBLE_SMOKE_COOKIE="$C" PRESEMBLE_SMOKE_SITE="$SITE_DIR" deno run --no-prompt --allow-net=127.0.0.1:$PORT --allow-env=PRESEMBLE_SMOKE_BASE,PRESEMBLE_SMOKE_COOKIE,PRESEMBLE_SMOKE_SITE --allow-read="$SITE_DIR/content/post",resources/serve/inject.js --allow-write="$SITE_DIR/content/post" tools/ws-smoke.ts 2>"$WS_ERR") || WS_RC=$?
WS_DONE=0
WS_FAILS=0
while IFS= read -r line; do
    case "$line" in
        "PASS "*) pass "${line#PASS }" ;;
        "FAIL "*) fail "${line#FAIL }"; WS_FAILS=$((WS_FAILS + 1)) ;;
        DONE) WS_DONE=1 ;;
    esac
done <<< "$WS_OUT"
if [ "$WS_DONE" -eq 0 ]; then
    fail "ws-smoke.ts did not print DONE (exit $WS_RC)"
    cat "$WS_ERR"
elif [ "$WS_RC" -ne 0 ] && [ "$WS_FAILS" -eq 0 ]; then
    fail "ws-smoke.ts exited $WS_RC without reporting a FAIL"
    cat "$WS_ERR"
fi

# ── Test: Create content ──────────────────────────────────────────────────
# After the WebSocket section on purpose: a new file is empty (incomplete
# content), and an incomplete document keeps the whole site from being
# published. The WebSocket section checks that saving reaches the readers, which
# needs a site that passes the publish gate.

log "Creating content..."
assert_curl "create author" "/_presemble/create-content" POST '{"stem":"author","slug":"alice"}' '"ok":true'
assert_curl "create post" "/_presemble/create-content" POST '{"stem":"post","slug":"first-post"}' '"ok":true'

# ── Test: Edit content ────────────────────────────────────────────────────

log "Editing content..."
# A new file is empty: each slot is a placeholder in the editorial page, and a
# block edit with an empty base fills it (ADR-059 section 10).
block_edit "fill the empty author name slot" "/author/alice" content/author/alice.md name "Alice Smith"
block_edit "fill the empty post title slot" "/post/first-post" content/post/first-post.md title "My First Post"

# ── Test: Structure mode rendering via render endpoint ────────────────────

log "Testing schema render endpoint..."

# View mode: re-renders the content page (sanity check that the endpoint works)
assert_curl "render endpoint view mode renders content" \
    "/_presemble/render?path=/post/hello-world&mode=view" GET "" \
    'data-presemble-slot="title"'

# Schema mode: synthesizes empty document and renders schema-as-mockup
assert_curl "render endpoint schema mode emits constraint attrs" \
    "/_presemble/render?path=/_schema/post/item&mode=schema" GET "" \
    'data-presemble-schema-constraints-occurs'

assert_curl "render endpoint schema mode emits instance count attr" \
    "/_presemble/render?path=/_schema/post/item&mode=schema" GET "" \
    'data-presemble-schema-instance-count'

assert_curl "render endpoint schema mode emits included-by attr" \
    "/_presemble/render?path=/_schema/author/item&mode=schema" GET "" \
    'data-presemble-schema-included-by'

# Schema mode at root: synthesises root index schema
assert_curl "render endpoint schema mode at root emits constraints" \
    "/_presemble/render?path=/_schema/index&mode=schema" GET "" \
    'data-presemble-schema-constraints'

# Subschema link: post's author slot points at /author/#_schema in schema mode
assert_curl "render endpoint schema mode emits typelink href" \
    "/_presemble/render?path=/_schema/post/item&mode=schema" GET "" \
    '/author/#_schema'

# Bad mode: should return 400
assert_status "render endpoint rejects bad mode" \
    "/_presemble/render?path=/post/hello-world&mode=invalid" "400"

# Missing path: Axum query-param deserialisation returns 400
assert_status "render endpoint rejects missing path" \
    "/_presemble/render?mode=schema" "400"

# ── Test: Schema-for endpoint ────────────────────────────────────────────

log "Testing schema-for endpoint..."
assert_curl "schema-for endpoint resolves item URL" \
    "/_presemble/schema-for?page=/post/hello-world" GET "" '"/_schema/post/item"'
assert_curl "schema-for endpoint resolves collection URL" \
    "/_presemble/schema-for?page=/post/" GET "" '"/_schema/post/index"'
assert_curl "schema-for endpoint resolves root URL" \
    "/_presemble/schema-for?page=/" GET "" '"/_schema/index"'
assert_status "schema-for endpoint returns 404 for unknown page" \
    "/_presemble/schema-for?page=/no/such/page" "404"

# ── Test: Page-for endpoint ───────────────────────────────────────────────

log "Testing page-for endpoint..."
assert_curl "page-for endpoint resolves item schema" \
    "/_presemble/page-for?schema=/_schema/post/item" GET "" '"/post/'
assert_curl "page-for endpoint resolves collection schema" \
    "/_presemble/page-for?schema=/_schema/post/index" GET "" '"/post/"'
assert_curl "page-for endpoint resolves root schema" \
    "/_presemble/page-for?schema=/_schema/index" GET "" '"/"'
assert_status "page-for endpoint returns 404 for unknown schema" \
    "/_presemble/page-for?schema=/_schema/no-such-stem/item" "404"

# ── Test: Direct GET to schema URL ────────────────────────────────────────

log "Testing direct schema URL rendering..."
assert_curl "direct GET to /_schema/post/item renders schema" \
    "/_schema/post/item" GET "" 'data-presemble-schema-constraints'
assert_curl "direct GET to /_schema/post/index renders schema" \
    "/_schema/post/index" GET "" 'data-presemble-schema-constraints'
assert_curl "direct GET to /_schema/index renders root schema" \
    "/_schema/index" GET "" 'data-presemble-schema-constraints'

# ── Test: Save edits to disk for nREPL tests ─────────────────────────────

log "Saving edits to disk for nREPL..."
assert_curl "save all before nrepl" "/_presemble/save-all" POST "" '"ok":true'
sleep 1

# ── Test: nREPL (if rep is available) ─────────────────────────────────────

# nREPL port file is written to site_dir's parent
NREPL_PORT_DIR="$(dirname "$SITE_DIR")"
NREPL_PORT_FILE="$NREPL_PORT_DIR/.nrepl-port"
if command -v rep &> /dev/null && [ -f "$NREPL_PORT_FILE" ]; then
    log "Testing nREPL via rep..."
    assert_rep "list-schemas returns post" "(list-schemas)" "post"
    assert_rep "list-content returns post" "(list-content)" "first-post"

    log "Testing edge queries via nREPL..."
    assert_rep "refs-from post has author edge" '(refs-from "/post/hello-world")' "author"
    assert_rep "refs-to author has post edge" '(refs-to "/author/default")' "post"
else
    log "Skipping nREPL tests (rep not available or $NREPL_PORT_FILE not found)"
fi

# ── Test: Duplicate prevention ────────────────────────────────────────────

log "Testing duplicate prevention..."
assert_curl "duplicate author rejected" "/_presemble/create-content" POST '{"stem":"author","slug":"alice"}' "already exists"
assert_curl "duplicate post rejected" "/_presemble/create-content" POST '{"stem":"post","slug":"first-post"}' "already exists"

# ── Test: Closed editorial plane ──────────────────────────────────────────
# MUST stay the last section: it restarts serve on the same port with no
# --dev-session and no PRESEMBLE_SESSION_KEY, so editing is disabled. The
# conductor daemon is reused.

log "Restarting serve with editing disabled (closed editorial plane)..."
# Readers get the published output, which exists only for a site that passes
# the publish gate. The empty author and post created above (to test duplicate
# prevention) are incomplete content, so they are removed before the restart:
# the restart then publishes the saved site.
rm -f "$SITE_DIR/content/author/alice.md" "$SITE_DIR/content/post/first-post.md"
kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
pkill -f "presemble serve $SITE_DIR" 2>/dev/null || true
for i in $(seq 1 20); do
    if ! curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
        break
    fi
    sleep 0.5
done

# Prints the response (body, or headers+body with -D -) followed by a final
# line holding the HTTP status. Split with ${R##*$'\n'} / ${R%$'\n'*}.
http_with_status() { curl -s -w '\n%{http_code}' "$@" 2>&1 || true; }

if curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
    fail "closed: port $PORT still in use 10s after stopping serve"
else
    env -u PRESEMBLE_SESSION_KEY cargo polylith cargo --profile "$PROFILE" run --bin presemble -- serve "$SITE_DIR/" > "$SITE_DIR/serve-closed.log" 2>&1 &
    PID=$!

    for i in $(seq 1 30); do
        if curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
            break
        fi
        sleep 0.5
    done

    if ! curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
        fail "closed: server failed to restart without a session store"
        cat "$SITE_DIR/serve-closed.log"
    else
        log "Server ready on port $PORT (editing disabled)"
        CLOSED_LOG=$(cat "$SITE_DIR/serve-closed.log")
        assert_contains "closed: banner says editing disabled" "$CLOSED_LOG" "Session store: none (editing disabled)"
        assert_not_contains "closed: no Login URL printed" "$CLOSED_LOG" "Login URL:"

        assert_status_anon "closed: published page still 200" GET "/post/hello-world" "200"
        B=$(curl -s "http://127.0.0.1:$PORT/post/hello-world" 2>&1 || true)
        assert_contains "anonymous page gets public live-reload socket" "$B" "/_presemble/ws"
        assert_not_contains "published page carries no editor attributes" "$B" "data-presemble-address"
        assert_status_anon "closed: the diagnostics probe is published" GET "/post/diag-probe" "200"
        B=$(curl -s "http://127.0.0.1:$PORT/post/diag-probe" 2>&1 || true)
        assert_not_contains "published probe carries no diagnostics attribute" "$B" "data-presemble-diagnostics"
        assert_not_contains "published probe carries no diagnostics script" "$B" 'id="presemble-diagnostics"'

        EDIT_JSON="$BLOCK_EDIT_PROBE"
        R=$(http_with_status -X POST "http://127.0.0.1:$PORT/_presemble/block-edit" -H 'Content-Type: application/json' -d "$EDIT_JSON")
        S="${R##*$'\n'}"; B="${R%$'\n'*}"
        if [ "$S" = "401" ]; then pass "closed: anonymous edit is 401"; else fail "closed: anonymous edit is 401 (got $S: $B)"; fi
        assert_contains "closed: anonymous edit says editing is disabled" "$B" "Editing is disabled"

        R=$(http_with_status -b "$COOKIE_JAR" -X POST "http://127.0.0.1:$PORT/_presemble/block-edit" -H 'Content-Type: application/json' -d "$EDIT_JSON")
        S="${R##*$'\n'}"; B="${R%$'\n'*}"
        if [ "$S" = "401" ]; then pass "closed: old dev cookie edit is 401"; else fail "closed: old dev cookie edit is 401 (got $S: $B)"; fi
        assert_contains "closed: old dev cookie edit says editing is disabled" "$B" "Editing is disabled"

        R=$(http_with_status -D - "http://127.0.0.1:$PORT/_presemble/login?token=bogus")
        S="${R##*$'\n'}"; B="${R%$'\n'*}"
        if [ "$S" = "401" ]; then pass "closed: login is 401"; else fail "closed: login is 401 (got $S: $B)"; fi
        assert_contains "closed: login says editing is disabled" "$B" "Editing is disabled"
        if grep -qi '^set-cookie:' <<< "$B"; then
            fail "closed: login sets no cookie (got: $B)"
        else
            pass "closed: login sets no cookie"
        fi

        # WebSocket split with editing disabled: editorial socket refused,
        # public live-reload socket still open, readers get no editor script.
        assert_ws_status "closed: anonymous editorial-ws upgrade is 401" "/_presemble/editorial-ws" "401"
        B=$(ws_upgrade "http://127.0.0.1:$PORT/_presemble/editorial-ws")
        assert_contains "closed: anonymous editorial-ws says editing is disabled" "$B" "Editing is disabled"
        assert_ws_status "closed: old dev cookie editorial-ws upgrade is 401" "/_presemble/editorial-ws" "401" -b "$COOKIE_JAR"
        B=$(ws_upgrade -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/_presemble/editorial-ws")
        assert_contains "closed: old dev cookie editorial-ws says editing is disabled" "$B" "Editing is disabled"
        assert_ws_status "closed: anonymous public ws upgrade is 101" "/_presemble/ws" "101"   # 101-on-timeout assumption
        B=$(curl -s "http://127.0.0.1:$PORT/post/hello-world" 2>&1 || true)
        assert_not_contains "closed: anonymous page has no editorial socket" "$B" "editorial-ws"
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────────

echo ""
echo -e "══════════════════════════════════════"
echo -e "  ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}"
if [ $FAIL -gt 0 ]; then
    echo -e "${RED}Failures:${ERRORS}${NC}"
fi
echo -e "══════════════════════════════════════"
exit $FAIL
