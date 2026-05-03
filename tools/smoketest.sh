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
    response=$(curl -s "http://127.0.0.1:$PORT$url" 2>&1)
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
    status=$(curl -s -o /dev/null -w "%{http_code}" "http://127.0.0.1:$PORT$url" 2>&1)
    if [ "$status" = "$expected" ]; then
        pass "$desc"
    else
        fail "$desc (expected HTTP $expected, got $status)"
    fi
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

# ── Test: nth-child scopes edit to one paragraph, leaves others intact ────

log "Testing nth-child slot edit scoping..."
NED_PROGRAM='(ned/set-text (-> (ned/nth-child (ned/slot (ned/doc-by-path "content/post/hello-world.md") "summary") 1) ned/descendants ned/texts) "NTHCHILD ONLY SECOND")'
# Build JSON by escaping the double quotes inside the program string
APPLY_BODY='{"program":"'"$(echo "$NED_PROGRAM" | sed 's/"/\\"/g')"'"}'
assert_curl "apply nth-child edit to 2nd paragraph" "/_presemble/apply" POST "$APPLY_BODY" '"dirtyPaths":1'
assert_curl "nth-child edit: 2nd paragraph updated" "/post/hello-world" GET "" "NTHCHILD ONLY SECOND"
assert_curl "nth-child edit: 1st paragraph unchanged" "/post/hello-world" GET "" "Welcome to your new blog"

# ── Test: Suggestions ─────────────────────────────────────────────────────

log "Testing suggestions..."
assert_curl "no suggestions initially" "/_presemble/suggestions?file=content/post/first-post.md" GET "" "[]"

# ── Test: Create suggestion ───────────────────────────────────────────────

log "Creating suggestion via MCP-style endpoint..."
# (suggestions are created via the conductor, not HTTP — skip for now unless MCP is connected)

# ── Test: NED suggestions via HTTP ────────────────────────────────────────

log "Testing NED suggestions via HTTP..."

NED_SUG_BODY='{"file":"content/post/hello-world.md","selection":"(ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")","mutation":{"SetText":"Suggested Title"},"reason":"smoketest"}'
NED_SUG_RESPONSE=$(curl -s -X POST "http://127.0.0.1:$PORT/_presemble/ned-suggestions" -H 'Content-Type: application/json' -d "$NED_SUG_BODY" 2>&1)

if echo "$NED_SUG_RESPONSE" | grep -qF '"ok":true'; then
    pass "ned_suggestions_create_returns_ok"
else
    fail "ned_suggestions_create_returns_ok (expected '\"ok\":true', got: $NED_SUG_RESPONSE)"
fi

if echo "$NED_SUG_RESPONSE" | grep -qF '"id":"sug-'; then
    pass "ned_suggestions_create_returns_id"
else
    fail "ned_suggestions_create_returns_id (expected '\"id\":\"sug-', got: $NED_SUG_RESPONSE)"
fi

# Capture the suggestion id for subsequent reject
NED_SUG_ID=$(echo "$NED_SUG_RESPONSE" | grep -o '"id":"[^"]*"' | head -1 | sed 's/"id":"//;s/"//')

assert_curl "ned_suggestions_list_for_file_reason" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"reason":"smoketest"'
assert_curl "ned_suggestions_list_for_file_anchor_kind" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"kind":"slot"'
assert_curl "ned_suggestions_list_for_file_anchor_slot" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"slot":"title"'

assert_curl "ned_suggestion_files_includes_file" "/_presemble/ned-suggestion-files" GET "" '"content/post/hello-world.md"'

NED_REJECT_BODY='{"id":"'"$NED_SUG_ID"'"}'
assert_curl "ned_suggestions_reject_marks_rejected" "/_presemble/ned-suggestions/reject" POST "$NED_REJECT_BODY" '"ok":true'

assert_curl "ned_suggestions_list_after_reject_contains_rejected" "/_presemble/ned-suggestions?file=content/post/hello-world.md" GET "" '"Rejected"'

NED_BAD_BODY='{"file":"content/post/hello-world.md","selection":"(ned/slot (ned/doc-by-path \"content/post/hello-world.md\") \"title\")","mutation":{"Replace":[{"Existing":0}]},"reason":"reject me"}'
assert_curl "ned_suggestions_create_rejects_existing_node_ok_false" "/_presemble/ned-suggestions" POST "$NED_BAD_BODY" '"ok":false'
assert_curl "ned_suggestions_create_rejects_existing_node_mentions_existing" "/_presemble/ned-suggestions" POST "$NED_BAD_BODY" 'Existing'

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

# ── Summary ───────────────────────────────────────────────────────────────

echo ""
echo -e "══════════════════════════════════════"
echo -e "  ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}"
if [ $FAIL -gt 0 ]; then
    echo -e "${RED}Failures:${ERRORS}${NC}"
fi
echo -e "══════════════════════════════════════"
exit $FAIL
