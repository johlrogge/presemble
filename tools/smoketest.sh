#!/usr/bin/env bash
# Presemble smoke test — exercises the full workflow via curl and rep.
# Usage: ./tools/smoketest.sh [dev|live]
# Default: dev profile

set -euo pipefail

PROFILE="${1:-dev}"
PORT=3000
SITE_DIR=""
PID=""
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
        rm -rf "$SITE_DIR"
    fi
    # Clean up nrepl port file
    rm -f "$(dirname "${SITE_DIR:-/tmp/x}")/.nrepl-port" 2>/dev/null || true
}
trap cleanup EXIT

log() { echo -e "${YELLOW}▸${NC} $1"; }
pass() { PASS=$((PASS + 1)); echo -e "  ${GREEN}✓${NC} $1"; }
fail() { FAIL=$((FAIL + 1)); ERRORS="${ERRORS}\n  ✗ $1"; echo -e "  ${RED}✗${NC} $1"; }

# Kill any stale presemble processes on our port
if curl -s "http://127.0.0.1:$PORT/" > /dev/null 2>&1; then
    log "Port $PORT is busy — killing stale processes..."
    pkill -f "presemble serve" 2>/dev/null || true
    pkill -f "presemble conductor" 2>/dev/null || true
    sleep 2
fi

# Assert a curl response contains expected text
assert_curl() {
    local desc="$1" url="$2" method="${3:-GET}" body="${4:-}" expected="$5"
    local response
    if [ "$method" = "POST" ]; then
        response=$(curl -s -X POST "http://127.0.0.1:$PORT$url" -H 'Content-Type: application/json' -d "$body" 2>&1)
    else
        response=$(curl -s "http://127.0.0.1:$PORT$url" 2>&1)
    fi
    if echo "$response" | grep -qF "$expected"; then
        pass "$desc"
    else
        fail "$desc (expected '$expected', got: $response)"
    fi
}

# Assert rep expression contains expected text
assert_rep() {
    local desc="$1" expr="$2" expected="$3"
    local response
    local port_dir="$(dirname "$SITE_DIR")"
    response=$(rep -p "@$port_dir/.nrepl-port" "$expr" 2>&1 || echo "REP_ERROR")
    if echo "$response" | grep -qF "$expected"; then
        pass "$desc"
    else
        fail "$desc (expected '$expected', got: $response)"
    fi
}

# ── Setup ──────────────────────────────────────────────────────────────────

log "Building presemble ($PROFILE profile)..."
cargo polylith cargo --profile "$PROFILE" build --bin presemble -q 2>/dev/null

SITE_DIR=$(mktemp -d /tmp/presemble-smoke-XXXXXX)
log "Site directory: $SITE_DIR"

# Start serve on a non-default port
log "Starting presemble serve..."
# Clean up stale nrepl-port files
rm -f "$(dirname "$SITE_DIR")/.nrepl-port"
cargo polylith cargo --profile "$PROFILE" run --bin presemble -- serve "$SITE_DIR/" > "$SITE_DIR/serve.log" 2>&1 &
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

# ── Test: Welcome page ─────────────────────────────────────────────────────

log "Testing welcome page..."
assert_curl "welcome page on empty site" "/" GET "" "Welcome to Presemble"

# ── Test: Scaffold blog ────────────────────────────────────────────────────

log "Scaffolding blog site..."
assert_curl "scaffold blog" "/_presemble/scaffold" POST '{"template":"blog","format":"hiccup"}' '"ok":true'
sleep 2

# ── Test: Schemas endpoint ─────────────────────────────────────────────────

log "Testing schemas..."
assert_curl "schemas list includes post" "/_presemble/schemas" GET "" "post"
assert_curl "schemas list includes author" "/_presemble/schemas" GET "" "author"

# ── Test: Index page built ─────────────────────────────────────────────────

log "Testing index page..."
assert_curl "index page exists" "/" GET "" "html"

# ── Test: Edit index tagline ───────────────────────────────────────────────

log "Editing index tagline..."
assert_curl "edit index tagline" "/_presemble/edit" POST '{"file":"content/index.md","slot":"tagline","value":"Smoke Test Blog"}' '"ok":true'

# ── Test: Dirty buffers ───────────────────────────────────────────────────

log "Testing dirty buffers..."
assert_curl "dirty buffers after edit" "/_presemble/dirty-buffers" GET "" "content/index.md"

# ── Test: Save all ─────────────────────────────────────────────────────────

log "Saving all buffers..."
assert_curl "save all" "/_presemble/save-all" POST "" '"ok":true'
assert_curl "no dirty buffers after save" "/_presemble/dirty-buffers" GET "" "[]"

# ── Test: Create content ──────────────────────────────────────────────────

log "Creating content..."
assert_curl "create author" "/_presemble/create-content" POST '{"stem":"author","slug":"alice"}' '"ok":true'
assert_curl "create post" "/_presemble/create-content" POST '{"stem":"post","slug":"first-post"}' '"ok":true'

# ── Test: Edit content ────────────────────────────────────────────────────

log "Editing content..."
assert_curl "edit author name" "/_presemble/edit" POST '{"file":"content/author/alice.md","slot":"name","value":"Alice Smith"}' '"ok":true'
assert_curl "edit post title" "/_presemble/edit" POST '{"file":"content/post/first-post.md","slot":"title","value":"My First Post"}' '"ok":true'

# ── Test: Suggestions ─────────────────────────────────────────────────────

log "Testing suggestions..."
assert_curl "no suggestions initially" "/_presemble/suggestions?file=content/post/first-post.md" GET "" "[]"

# ── Test: Create suggestion ───────────────────────────────────────────────

log "Creating suggestion via MCP-style endpoint..."
# (suggestions are created via the conductor, not HTTP — skip for now unless MCP is connected)

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
else
    log "Skipping nREPL tests (rep not available or $NREPL_PORT_FILE not found)"
fi

# ── Test: Duplicate prevention ────────────────────────────────────────────

log "Testing duplicate prevention..."
assert_curl "duplicate author rejected" "/_presemble/create-content" POST '{"stem":"author","slug":"alice"}' "already exists"
assert_curl "duplicate post rejected" "/_presemble/create-content" POST '{"stem":"post","slug":"first-post"}' "already exists"

# ── Summary ───────────────────────────────────────────────────────────────

echo ""
echo -e "══════════════════════════════════════"
echo -e "  ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}"
if [ $FAIL -gt 0 ]; then
    echo -e "${RED}Failures:${ERRORS}${NC}"
fi
echo -e "══════════════════════════════════════"
exit $FAIL
