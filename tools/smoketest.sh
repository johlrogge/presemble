#!/usr/bin/env bash
# Presemble smoke test — exercises the full workflow via curl and rep.
# Usage: ./tools/smoketest.sh [dev|live]
# Default: dev profile

set -euo pipefail

PROFILE="${1:-dev}"
PORT=3000
SITE_DIR=""
COOKIE_JAR=""
WS_ERR=""
PID=""
KEY_CHECK_DIR=""
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
    if deno check "$js_file" 2>&1 | tee /tmp/presemble-deno-check.log | grep -qE '^(error|TS[0-9]+)'; then
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
)

for ts_file in "${DENO_TOOL_FILES[@]}"; do
    if deno check "$ts_file" 2>&1 | tee /tmp/presemble-deno-check.log | grep -qE '^(error|TS[0-9]+)'; then
        fail "deno check $ts_file"
        cat /tmp/presemble-deno-check.log
        JS_CHECK_FAILED=1
    else
        pass "deno check $ts_file"
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

# Build the /apply JSON body exactly as inject.js bsave does (cljStr + program
# shape), so this exercises the browser's body-edit wire format.
# deno eval runs with all permissions implicitly, so no --allow-env is needed.
bsave_body() {
    SCHEMA_SRC="$1" SEL="$2" BVALUE="$3" deno eval '
const cljStr = (s) => "\"" + String(s).replace(/\\/g, "\\\\").replace(/"/g, "\\\"") + "\"";
const program = "(let [g (ned/parse-grammar " + cljStr(Deno.env.get("SCHEMA_SRC")) + ")]\n  (ned/replace\n    "
  + Deno.env.get("SEL") + "\n    (ned/parse-body " + cljStr(Deno.env.get("BVALUE")) + " g)))";
console.log(JSON.stringify({ program }));'
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
# Cookie jar lives outside $SITE_DIR so the file watcher never sees it.
COOKIE_JAR=$(mktemp /tmp/presemble-smoke-cookies-XXXXXX)
log "Site directory: $SITE_DIR"

# Seed legacy (pre-ADR-056) NED suggestions BEFORE serve starts — the
# conductor loads suggestions only at startup. Does not affect the empty-site
# welcome page: is_source_empty only inspects schemas/ and content/.
mkdir -p "$SITE_DIR/.presemble/suggestions/ned"
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
PROBE=$(curl -s -o /dev/null -w '%{http_code}' -b "$COOKIE_JAR" "http://127.0.0.1:$PORT/_presemble/dirty-buffers")
[ "$PROBE" = "200" ] || login_fail "authenticated probe returned $PROBE"
log "Logged in (dev session)"

# ── Test: Auth boundary (ADR-054) ──────────────────────────────────────────
# Plain curl, no cookie.

log "Testing auth boundary..."
assert_status_anon "editorial POST without session is 401" POST "/_presemble/edit" "401" \
    '{"file":"content/index.md","slot":"tagline","value":"x"}'
ANON_BODY=$(curl -s -X POST "http://127.0.0.1:$PORT/_presemble/edit" -H 'Content-Type: application/json' \
    -d '{"file":"content/index.md","slot":"tagline","value":"x"}' 2>&1 || true)
assert_contains "anonymous 401 explains not logged in" "$ANON_BODY" "Not logged in"
assert_not_contains "anonymous 401 (dev session on) does not suggest --dev-session" "$ANON_BODY" "--dev-session"
assert_status_anon "schema URL without session is 401" GET "/_schema/index" "401"
assert_status_anon "login with bogus token is 401" GET "/_presemble/login?token=bogus.token" "401"
assert_status_anon "published page without session is 200" GET "/" "200"

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
# Deliberately a raw /apply program (ned/set-text over descendants), NOT the
# browser's (ned/text-in ...) shape — it exercises the generic NED apply path.
NED_PROGRAM='(ned/set-text (-> (ned/nth-child (ned/slot (ned/doc-by-path "content/post/hello-world.md") "summary") 1) ned/descendants ned/texts) "NTHCHILD ONLY SECOND")'
# Build JSON by escaping the double quotes inside the program string
APPLY_BODY='{"program":"'"$(echo "$NED_PROGRAM" | sed 's/"/\\"/g')"'"}'
assert_curl "apply nth-child edit to 2nd paragraph" "/_presemble/apply" POST "$APPLY_BODY" '"dirtyPaths":1'
assert_curl "nth-child edit: 2nd paragraph updated" "/post/hello-world" GET "" "NTHCHILD ONLY SECOND"
assert_curl "nth-child edit: 1st paragraph unchanged" "/post/hello-world" GET "" "Welcome to your new blog"

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

# D. A browser-style body edit (the bsave shape) via /apply survives save
log "Testing bsave-shaped body edit..."
SCHEMA_SRC=$(acurl "http://127.0.0.1:$PORT/_presemble/grammar?stem=post")
assert_contains "grammar for post fetched" "$SCHEMA_SRC" "{#title}"
BSAVE_SEL='(ned/nth-after (ned/heading-with-text (ned/children (ned/slot (ned/doc-by-path "content/post/hello-world.md") "body")) "What to Edit") "paragraph" 0)'
BSAVE_BODY=$(bsave_body "$SCHEMA_SRC" "$BSAVE_SEL" "Smoke body edit via bsave.") || {
    fail "deno eval failed to build bsave body"
    BSAVE_BODY='{}'
}
R=$(post_json /_presemble/apply "$BSAVE_BODY")
assert_contains "bsave-shaped apply ok" "$R" '"ok":true'
assert_contains "bsave-shaped apply dirties one doc" "$R" '"dirtyPaths":1'

assert_curl "bsave edit rendered" "/post/hello-world" GET "" "Smoke body edit via bsave."
assert_curl "sibling heading intact" "/post/hello-world" GET "" "What to Edit"
assert_curl "other body para intact" "/post/hello-world" GET "" "Open the"
assert_curl "bsave marks dirty" "/_presemble/dirty-buffers" GET "" "content/post/hello-world.md"
assert_curl "save after bsave" "/_presemble/save-all" POST "" '"ok":true'

F="$SITE_DIR/content/post/hello-world.md"
if grep -qF 'Smoke body edit via bsave.' "$F"; then
    pass "bsave edit survives save"
else
    fail "bsave edit survives save (not found in $F)"
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
assert_curl "no dirty buffers after bsave save" "/_presemble/dirty-buffers" GET "" "[]"

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
# written in D. It prints PASS/FAIL lines and a final DONE on stdout.

log "Testing WebSocket broadcasts..."
# curl writes HttpOnly cookies as "#HttpOnly_<domain>", so '#' lines must NOT
# be filtered out here.
C=$(awk '$6=="presemble_session"{print $7}' "$COOKIE_JAR")
WS_ERR=$(mktemp /tmp/presemble-smoke-wserr-XXXXXX)
WS_RC=0
WS_OUT=$(PRESEMBLE_SMOKE_BASE="http://127.0.0.1:$PORT" PRESEMBLE_SMOKE_COOKIE="$C" deno run --no-prompt --allow-net=127.0.0.1:$PORT --allow-env=PRESEMBLE_SMOKE_BASE,PRESEMBLE_SMOKE_COOKIE tools/ws-smoke.ts 2>"$WS_ERR") || WS_RC=$?
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

        EDIT_JSON='{"file":"content/index.md","slot":"tagline","value":"x"}'
        R=$(http_with_status -X POST "http://127.0.0.1:$PORT/_presemble/edit" -H 'Content-Type: application/json' -d "$EDIT_JSON")
        S="${R##*$'\n'}"; B="${R%$'\n'*}"
        if [ "$S" = "401" ]; then pass "closed: anonymous edit is 401"; else fail "closed: anonymous edit is 401 (got $S: $B)"; fi
        assert_contains "closed: anonymous edit says editing is disabled" "$B" "Editing is disabled"

        R=$(http_with_status -b "$COOKIE_JAR" -X POST "http://127.0.0.1:$PORT/_presemble/edit" -H 'Content-Type: application/json' -d "$EDIT_JSON")
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
