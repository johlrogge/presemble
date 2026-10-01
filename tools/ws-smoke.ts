// ws-smoke.ts — WebSocket smoke test for `/_presemble/ws`.
//
// Covers what curl cannot: live-reload and suggestion-list broadcasts that the
// serve process pushes over the browser socket after editorial HTTP actions.
//
// Usage (run by tools/smoketest.sh after its earlier sections):
//
//   PRESEMBLE_SMOKE_BASE=http://127.0.0.1:3000 \
//   PRESEMBLE_SMOKE_COOKIE=<raw presemble_session value> \
//   deno run --allow-net --allow-env tools/ws-smoke.ts
//
// Inputs come from the environment (never argv) so the session token does not
// show up in `ps`.
//
// Output contract:
//   stdout: `PASS <desc>` / `FAIL <desc>: <detail>` lines, then `DONE` last.
//   stderr: diagnostics (skipped messages, drained events, response bodies).
//   exit:   0 if everything passed, 1 otherwise. Stops at the first FAIL —
//           later expectations are not trustworthy after a missed message.
//
// Deliberately import-free: no network or module-cache dependency.

// ── Wire model ──────────────────────────────────────────────────────────────

/** Structural anchor as sent by the server; only passed through, not inspected. */
type Anchor = Readonly<Record<string, unknown>>;

interface Reload {
  readonly type: "reload";
  readonly pages: readonly string[];
  /** `pages[0]`, or `""` when `pages` is empty. */
  readonly primary: string;
  readonly anchor: Anchor | null;
}

interface SuggestionListChanged {
  readonly type: "suggestion-list-changed";
  /** Omitted on the wire when the server has no file; modelled as `null`. */
  readonly file: string | null;
}

interface Resync {
  readonly type: "resync";
}

interface Scroll {
  readonly type: "scroll";
  readonly anchor: Anchor;
}

type WsMessage = Reload | SuggestionListChanged | Resync | Scroll;

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isStringArray(v: unknown): v is string[] {
  return Array.isArray(v) && v.every((x: unknown) => typeof x === "string");
}

/** Parse one socket frame into a typed message; `null` if it is not one we know. */
function parseWsMessage(raw: string): WsMessage | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!isRecord(parsed)) return null;

  switch (parsed.type) {
    case "reload": {
      const { pages, primary, anchor } = parsed;
      if (!isStringArray(pages) || typeof primary !== "string") return null;
      if (anchor !== undefined && !isRecord(anchor)) return null;
      return { type: "reload", pages, primary, anchor: anchor ?? null };
    }
    case "suggestion-list-changed": {
      const { file } = parsed;
      if (file !== undefined && typeof file !== "string") return null;
      return { type: "suggestion-list-changed", file: file ?? null };
    }
    case "resync":
      return { type: "resync" };
    case "scroll": {
      const { anchor } = parsed;
      if (!isRecord(anchor)) return null;
      return { type: "scroll", anchor };
    }
    default:
      return null;
  }
}

// ── Reporting ───────────────────────────────────────────────────────────────

class StepFailure extends Error {
  constructor(readonly desc: string, readonly detail: string) {
    super(`${desc}: ${detail}`);
  }
}

function pass(desc: string): void {
  console.log(`PASS ${desc}`);
}

function diag(msg: string): void {
  console.error(`[ws-smoke] ${msg}`);
}

function check(desc: string, cond: boolean, detail: string): void {
  if (!cond) throw new StepFailure(desc, detail);
  pass(desc);
}

let finished = false;

/** Print the terminal `DONE` exactly once and exit. */
function finish(code: number): never {
  if (!finished) {
    finished = true;
    console.log("DONE");
  }
  Deno.exit(code);
}

// ── Message buffer ──────────────────────────────────────────────────────────

class Inbox {
  private readonly buf: WsMessage[] = [];
  private cursor = 0;
  private waiters: Array<() => void> = [];
  private lastActivity = Date.now();
  private closedReason: string | null = null;

  push(m: WsMessage): void {
    this.buf.push(m);
    this.lastActivity = Date.now();
    this.wake();
  }

  close(reason: string): void {
    if (this.closedReason === null) this.closedReason = reason;
    this.wake();
  }

  private wake(): void {
    const ws = this.waiters;
    this.waiters = [];
    for (const w of ws) w();
  }

  /** Resolve on the next push/close, or after `ms`, whichever comes first. */
  private waitForEvent(ms: number): Promise<void> {
    return new Promise<void>((resolve) => {
      const timer = setTimeout(resolve, Math.max(0, ms));
      this.waiters.push(() => {
        clearTimeout(timer);
        resolve();
      });
    });
  }

  /**
   * Wait for `silenceMs` without messages (capped at `capMs`), then discard
   * everything buffered so far. Drains stray rebuild events left over from
   * earlier smoketest sections.
   */
  async quiesce(silenceMs = 300, capMs = 3000): Promise<void> {
    const start = Date.now();
    this.lastActivity = Math.max(this.lastActivity, start);
    for (;;) {
      const now = Date.now();
      const quietFor = now - this.lastActivity;
      if (quietFor >= silenceMs) break;
      const elapsed = now - start;
      if (elapsed >= capMs) {
        diag(`quiesce: cap of ${capMs}ms reached without ${silenceMs}ms of silence`);
        break;
      }
      await this.waitForEvent(Math.min(silenceMs - quietFor, capMs - elapsed));
    }
    for (; this.cursor < this.buf.length; this.cursor++) {
      diag(`quiesce: drained ${JSON.stringify(this.buf[this.cursor])}`);
    }
  }

  /**
   * Consume messages from the cursor until one satisfies `predicate`.
   * Non-matching messages are skipped (and logged). Prints PASS on match;
   * throws StepFailure on timeout or socket close.
   */
  async expect<T extends WsMessage>(
    desc: string,
    predicate: (m: WsMessage) => m is T,
    timeoutMs = 5000,
  ): Promise<T> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      while (this.cursor < this.buf.length) {
        const m = this.buf[this.cursor++];
        if (predicate(m)) {
          pass(desc);
          return m;
        }
        diag(`skipped while waiting for "${desc}": ${JSON.stringify(m)}`);
      }
      if (this.closedReason !== null) {
        throw new StepFailure(desc, `socket closed (${this.closedReason})`);
      }
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new StepFailure(desc, `no matching message within ${timeoutMs}ms`);
      }
      await this.waitForEvent(remaining);
    }
  }
}

// ── Predicates ──────────────────────────────────────────────────────────────

function reloadIncluding(page: string): (m: WsMessage) => m is Reload {
  return (m: WsMessage): m is Reload => m.type === "reload" && m.pages.includes(page);
}

function suggestionListChangedFor(file: string): (m: WsMessage) => m is SuggestionListChanged {
  return (m: WsMessage): m is SuggestionListChanged =>
    m.type === "suggestion-list-changed" && m.file === file;
}

// ── Socket ──────────────────────────────────────────────────────────────────

/**
 * Open the browser socket. `auth` is `null` today: `/_presemble/ws` is public
 * (ADR-054). It exists so the planned split into a public live-reload socket
 * and a session-gated editorial socket only has to fill in the auth branch.
 */
function openSocket(url: string, auth: { cookie: string } | null): WebSocket {
  if (auth !== null) {
    // TODO(ws split): send `cookie: presemble_session=<auth.cookie>` on the
    // upgrade request. The standard WebSocket constructor cannot set headers.
    // Deno offers non-standard routes: `new WebSocket(url, { headers })` and
    // `WebSocketStream` (`new WebSocketStream(url, { headers })`). Whoever does
    // the split must verify which form exists and type-checks under the deno
    // version pinned in devenv before relying on it.
    throw new Error(
      "openSocket: authenticated sockets are not implemented yet (see TODO in tools/ws-smoke.ts)",
    );
  }
  return new WebSocket(url);
}

function waitOpen(ws: WebSocket, timeoutMs: number): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`not open within ${timeoutMs}ms`)), timeoutMs);
    ws.addEventListener("open", () => {
      clearTimeout(timer);
      resolve();
    });
    ws.addEventListener("error", () => {
      clearTimeout(timer);
      reject(new Error("socket error before open"));
    });
    ws.addEventListener("close", (ev: CloseEvent) => {
      clearTimeout(timer);
      reject(new Error(`socket closed before open (code ${ev.code})`));
    });
  });
}

// ── HTTP ────────────────────────────────────────────────────────────────────

interface HttpResult {
  readonly status: number;
  readonly text: string;
  /** Parsed body, or `undefined` if the body is not JSON. */
  readonly json: unknown;
}

class Http {
  constructor(private readonly base: string, private readonly cookie: string) {}

  async request(method: "GET" | "POST", path: string, body?: unknown): Promise<HttpResult> {
    const headers: Record<string, string> = { cookie: `presemble_session=${this.cookie}` };
    const init: RequestInit = { method, headers, signal: AbortSignal.timeout(10000) };
    if (body !== undefined) {
      headers["content-type"] = "application/json";
      init.body = JSON.stringify(body);
    }
    const res = await fetch(`${this.base}${path}`, init);
    const text = await res.text();
    let json: unknown = undefined;
    try {
      json = JSON.parse(text);
    } catch {
      // Non-JSON body; callers report `text`.
    }
    diag(`${method} ${path} -> ${res.status} ${text}`);
    return { status: res.status, text, json };
  }

  post(path: string, body: unknown): Promise<HttpResult> {
    return this.request("POST", path, body);
  }

  get(path: string): Promise<HttpResult> {
    return this.request("GET", path);
  }
}

function isOk(r: HttpResult): boolean {
  return isRecord(r.json) && r.json.ok === true;
}

function expectOk(desc: string, r: HttpResult): void {
  check(desc, isOk(r), `expected "ok":true, got HTTP ${r.status}: ${r.text}`);
}

/** Create a suggestion, assert `ok:true`, return its id. */
async function createSuggestion(
  http: Http,
  desc: string,
  body: Record<string, unknown>,
): Promise<string> {
  const r = await http.post("/_presemble/ned-suggestions", body);
  expectOk(desc, r);
  const id = isRecord(r.json) ? r.json.id : undefined;
  if (typeof id !== "string" || id === "") {
    throw new StepFailure(`${desc} (id)`, `no "id" in response: ${r.text}`);
  }
  return id;
}

// ── Scenario ────────────────────────────────────────────────────────────────

const FILE = "content/post/hello-world.md";
const PAGE = "/post/hello-world";

const TITLE_SELECTION =
  `(ned/text-in (ned/children (ned/slot (ned/doc-by-path "${FILE}") "title")))`;
const BODY_PARA_SELECTION =
  `(ned/text-containing (ned/body (ned/doc-by-path "${FILE}")) "Smoke body edit via bsave")`;

async function run(baseUrl: string, cookie: string): Promise<void> {
  const http = new Http(baseUrl, cookie);
  const wsUrl = `${baseUrl.replace(/^http/, "ws")}/_presemble/ws`;
  const inbox = new Inbox();

  // 1. Anonymous connection to the public live-reload socket.
  const openDesc = "ws: public live-reload socket accepts anonymous connection (ADR-054)";
  let ws: WebSocket;
  try {
    ws = openSocket(wsUrl, null);
  } catch (e) {
    throw new StepFailure(openDesc, String(e));
  }
  ws.onmessage = (ev: MessageEvent) => {
    const data: unknown = ev.data;
    if (typeof data !== "string") {
      diag(`ignoring non-text frame`);
      return;
    }
    const m = parseWsMessage(data);
    if (m === null) {
      diag(`unrecognised frame: ${data}`);
      return;
    }
    inbox.push(m);
  };
  ws.addEventListener("close", (ev: CloseEvent) => inbox.close(`code ${ev.code}`));
  try {
    await waitOpen(ws, 5000);
  } catch (e) {
    throw new StepFailure(openDesc, e instanceof Error ? e.message : String(e));
  }
  pass(openDesc);

  try {
    await inbox.quiesce();

    // 2. /edit → reload of the post page.
    {
      const r = await http.post("/_presemble/edit", {
        file: FILE,
        slot: "title",
        value: "WS Edit Title",
      });
      expectOk("ws: /edit returns ok", r);
      const m = await inbox.expect(`ws: /edit broadcasts reload including ${PAGE}`, reloadIncluding(PAGE));
      check(
        "ws: /edit reload primary is pages[0]",
        m.primary === m.pages[0],
        `primary=${JSON.stringify(m.primary)} pages=${JSON.stringify(m.pages)}`,
      );
    }

    // 3. /apply → reload of the post page.
    {
      const program =
        `(ned/set-text (-> (ned/nth-child (ned/slot (ned/doc-by-path "${FILE}") "summary") 1) ` +
        `ned/descendants ned/texts) "WS APPLY SECOND")`;
      const r = await http.post("/_presemble/apply", { program });
      expectOk("ws: /apply returns ok", r);
      const dirty = isRecord(r.json) ? r.json.dirtyPaths : undefined;
      check("ws: /apply dirties one doc", dirty === 1, `dirtyPaths=${JSON.stringify(dirty)}: ${r.text}`);
      await inbox.expect(`ws: /apply broadcasts reload including ${PAGE}`, reloadIncluding(PAGE));
    }

    // 4. Create + accept (re-renders) → suggestion-list-changed, then reload.
    {
      const id = await createSuggestion(http, "ws: create accept-suggestion returns ok", {
        file: FILE,
        selection: TITLE_SELECTION,
        mutation: { SetText: "WS Accepted Title" },
        reason: "ws smoke accept",
      });
      await inbox.expect(
        "ws: create broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );
      const r = await http.post("/_presemble/ned-suggestions/accept", { id });
      expectOk("ws: accept returns ok", r);
      const m = await inbox.expect(
        `ws: accept broadcasts reload including ${PAGE}`,
        reloadIncluding(PAGE),
      );
      check("ws: accept reload has non-empty pages", m.pages.length > 0, JSON.stringify(m));
    }

    // 5. Create + reject → suggestion-list-changed twice.
    {
      const id = await createSuggestion(http, "ws: create reject-suggestion returns ok", {
        file: FILE,
        selection: TITLE_SELECTION,
        mutation: { SetText: "WS Rejected Title" },
        reason: "ws smoke reject",
      });
      await inbox.expect(
        "ws: create (reject case) broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );
      const r = await http.post("/_presemble/ned-suggestions/reject", { id });
      expectOk("ws: reject returns ok", r);
      await inbox.expect(
        "ws: reject broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );
    }

    // 6. Stale: T rewrites the paragraph S targets; accepting S afterwards
    //    stales it → suggestion-list-changed (HTTP still ok:true).
    {
      const sId = await createSuggestion(http, "ws: create stale-candidate S returns ok", {
        file: FILE,
        selection: BODY_PARA_SELECTION,
        mutation: { SearchReplace: { search: "Smoke", replace: "Stale" } },
        reason: "ws smoke stale S",
      });
      await inbox.expect(
        "ws: create S broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );
      const tId = await createSuggestion(http, "ws: create rewriter T returns ok", {
        file: FILE,
        selection: BODY_PARA_SELECTION,
        mutation: {
          SearchReplace: {
            search: "Smoke body edit via bsave.",
            replace: "WS rewrote this paragraph.",
          },
        },
        reason: "ws smoke stale T",
      });
      await inbox.expect(
        "ws: create T broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );

      const rt = await http.post("/_presemble/ned-suggestions/accept", { id: tId });
      expectOk("ws: accept T returns ok", rt);
      await inbox.expect(`ws: accept T broadcasts reload including ${PAGE}`, reloadIncluding(PAGE));

      const rs = await http.post("/_presemble/ned-suggestions/accept", { id: sId });
      expectOk("ws: accept S (goes stale) still returns ok", rs);
      await inbox.expect(
        "ws: stale accept broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );

      // The entry's `status` serialises as `{"Stale":{...}}`. Check `status`
      // itself: S's own mutation contains the string "Stale" (replace text),
      // so a whole-entry substring check would pass vacuously.
      const rl = await http.get(`/_presemble/ned-suggestions?file=${encodeURIComponent(FILE)}`);
      const list = Array.isArray(rl.json) ? rl.json : [];
      const entry: unknown = list.find((e: unknown) => isRecord(e) && e.id === sId);
      const status = isRecord(entry) ? entry.status : undefined;
      check(
        "ws: S listed with Stale status",
        isRecord(status) && "Stale" in status,
        `entry for ${sId}: ${JSON.stringify(entry)} (HTTP ${rl.status})`,
      );
    }
  } finally {
    ws.close();
  }
}

// ── Entry point ─────────────────────────────────────────────────────────────

async function main(): Promise<void> {
  const guard = setTimeout(() => {
    console.log("FAIL ws-smoke overall timeout");
    finish(1);
  }, 60000);

  const base = Deno.env.get("PRESEMBLE_SMOKE_BASE");
  const cookie = Deno.env.get("PRESEMBLE_SMOKE_COOKIE");
  if (base === undefined || base === "" || cookie === undefined || cookie === "") {
    console.log(
      "FAIL ws-smoke config: PRESEMBLE_SMOKE_BASE and PRESEMBLE_SMOKE_COOKIE must be set",
    );
    clearTimeout(guard);
    finish(1);
  }

  let code = 0;
  try {
    await run(base.replace(/\/+$/, ""), cookie);
  } catch (e) {
    code = 1;
    if (e instanceof StepFailure) {
      console.log(`FAIL ${e.desc}: ${e.detail}`);
    } else {
      console.log(`FAIL ws-smoke unexpected error: ${e instanceof Error ? e.message : String(e)}`);
      if (e instanceof Error && e.stack !== undefined) diag(e.stack);
    }
  }
  clearTimeout(guard);
  finish(code);
}

main();
