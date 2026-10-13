// ws-smoke.ts — WebSocket smoke test for the two browser sockets:
//
//   /_presemble/ws            public, anonymous: reload (pages only) + resync;
//                             readers follow saves and file changes, never an
//                             edit in progress
//   /_presemble/editorial-ws  session cookie: reload with primary/anchor,
//                             scroll, suggestion-list-changed, conflict,
//                             save-state, resync
//
// Covers what curl cannot: live-reload and suggestion-list broadcasts that the
// serve process pushes over the browser sockets after editorial HTTP actions,
// and that no editorial message leaks onto the public socket.
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
  /**
   * Editorial socket: the page of the edited document, or `""` when no
   * edit caused the rebuild (a save, a file change).
   * Public socket: omitted on the wire; modelled as `null`.
   */
  readonly primary: string | null;
  /** Omitted on the wire when there is no anchor; modelled as `null`. */
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

/**
 * A file changed on disk while it has unsaved edits (ADR-065): as a whole, or
 * on some blocks (amendment F2). Editorial only.
 */
interface Conflict {
  readonly type: "conflict";
  readonly file: string;
  readonly state: "whole-file" | "blocks";
}

/** A file's conflict with its unsaved edits has ended. Editorial only. */
interface ConflictResolved {
  readonly type: "conflict-resolved";
  readonly file: string;
}

/**
 * A document's save state changed (ADR-070). Editorial only. `at` (saved
 * only) is milliseconds since the Unix epoch; `error` (failed only) says why.
 */
interface SaveStateMessage {
  readonly type: "save-state";
  readonly file: string;
  readonly state: "saved" | "unsaved" | "conflict" | "failed";
  readonly at: number | null;
  readonly error: string | null;
}

type WsMessage =
  | Reload
  | SuggestionListChanged
  | Resync
  | Scroll
  | Conflict
  | ConflictResolved
  | SaveStateMessage;

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
      if (!isStringArray(pages)) return null;
      if (primary !== undefined && typeof primary !== "string") return null;
      if (anchor !== undefined && !isRecord(anchor)) return null;
      return { type: "reload", pages, primary: primary ?? null, anchor: anchor ?? null };
    }
    case "suggestion-list-changed": {
      const { file } = parsed;
      if (file !== undefined && typeof file !== "string") return null;
      return { type: "suggestion-list-changed", file: file ?? null };
    }
    case "resync":
      return { type: "resync" };
    case "conflict": {
      const { file, state } = parsed;
      if (typeof file !== "string") return null;
      if (state !== "whole-file" && state !== "blocks") return null;
      return { type: "conflict", file, state };
    }
    case "conflict-resolved": {
      const { file } = parsed;
      if (typeof file !== "string") return null;
      return { type: "conflict-resolved", file };
    }
    case "scroll": {
      const { anchor } = parsed;
      if (!isRecord(anchor)) return null;
      return { type: "scroll", anchor };
    }
    case "save-state": {
      const { file, state, at, error } = parsed;
      if (typeof file !== "string") return null;
      switch (state) {
        case "saved":
          if (typeof at !== "number" || error !== undefined) return null;
          return { type: "save-state", file, state, at, error: null };
        case "failed":
          if (typeof error !== "string" || at !== undefined) return null;
          return { type: "save-state", file, state, at: null, error };
        case "unsaved":
        case "conflict":
          if (at !== undefined || error !== undefined) return null;
          return { type: "save-state", file, state, at: null, error: null };
        default:
          return null;
      }
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
  private violation: string | null = null;

  /** `name` labels diagnostics and failures ("public" / "editorial"). */
  constructor(readonly name: string) {}

  push(m: WsMessage): void {
    this.buf.push(m);
    this.lastActivity = Date.now();
    this.wake();
  }

  close(reason: string): void {
    if (this.closedReason === null) this.closedReason = reason;
    this.wake();
  }

  /**
   * Record a frame this socket must never carry. The next `quiesce`/`expect`
   * (and `assertNoViolation`) fails with it.
   */
  protocolViolation(detail: string): void {
    if (this.violation === null) this.violation = detail;
    this.wake();
  }

  /** Every message received so far, including ones already consumed or drained. */
  seen(): readonly WsMessage[] {
    return this.buf;
  }

  assertNoViolation(desc: string): void {
    if (this.violation !== null) {
      throw new StepFailure(desc, `${this.name} socket: ${this.violation}`);
    }
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
        diag(`quiesce (${this.name}): cap of ${capMs}ms reached without ${silenceMs}ms of silence`);
        break;
      }
      await this.waitForEvent(Math.min(silenceMs - quietFor, capMs - elapsed));
    }
    for (; this.cursor < this.buf.length; this.cursor++) {
      diag(`quiesce (${this.name}): drained ${JSON.stringify(this.buf[this.cursor])}`);
    }
    this.assertNoViolation(`ws: ${this.name} socket sent only recognised frames`);
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
      this.assertNoViolation(desc);
      while (this.cursor < this.buf.length) {
        const m = this.buf[this.cursor++];
        if (predicate(m)) {
          pass(desc);
          return m;
        }
        diag(`(${this.name}) skipped while waiting for "${desc}": ${JSON.stringify(m)}`);
      }
      if (this.closedReason !== null) {
        throw new StepFailure(desc, `${this.name} socket closed (${this.closedReason})`);
      }
      const remaining = deadline - Date.now();
      if (remaining <= 0) {
        throw new StepFailure(desc, `no matching message on ${this.name} socket within ${timeoutMs}ms`);
      }
      await this.waitForEvent(remaining);
    }
  }
}

// ── Predicates ──────────────────────────────────────────────────────────────

function reloadIncluding(page: string): (m: WsMessage) => m is Reload {
  return (m: WsMessage): m is Reload => m.type === "reload" && m.pages.includes(page);
}

function anchoredReloadIncluding(page: string): (m: WsMessage) => m is Reload {
  return (m: WsMessage): m is Reload =>
    m.type === "reload" && m.pages.includes(page) && m.anchor !== null;
}

/**
 * Readers follow saves, not edits in progress: after an unsaved change the
 * public socket stays silent. Waits long enough for the published build's
 * debounce (400 ms) and a build to have announced something, were it going to.
 */
async function expectNoPublicMessage(desc: string, inbox: Inbox, waitMs = 1500): Promise<void> {
  const before = inbox.seen().length;
  await new Promise<void>((resolve) => setTimeout(resolve, waitMs));
  const extra = inbox.seen().slice(before);
  check(desc, extra.length === 0, `public socket received ${JSON.stringify(extra)}`);
}

/** A message that must never appear on the public socket (ADR-054). */
function isEditorialOnly(m: WsMessage): boolean {
  switch (m.type) {
    case "scroll":
    case "suggestion-list-changed":
    case "conflict":
    case "conflict-resolved":
    case "save-state":
      return true;
    case "reload":
      return m.anchor !== null || m.primary !== null;
    case "resync":
      return false;
  }
}

function conflictFor(file: string): (m: WsMessage) => m is Conflict {
  return (m: WsMessage): m is Conflict => m.type === "conflict" && m.file === file;
}

function conflictResolvedFor(file: string): (m: WsMessage) => m is ConflictResolved {
  return (m: WsMessage): m is ConflictResolved =>
    m.type === "conflict-resolved" && m.file === file;
}

function saveStateFor(
  file: string,
  state: SaveStateMessage["state"],
): (m: WsMessage) => m is SaveStateMessage {
  return (m: WsMessage): m is SaveStateMessage =>
    m.type === "save-state" && m.file === file && m.state === state;
}

/**
 * The id of the first conflict the editorial page marks a block of `file`
 * with (`data-presemble-conflict`, a JSON array of {id, file, ...}).
 */
function conflictIdOnPage(html: string, file: string): string | null {
  for (const match of html.matchAll(/data-presemble-conflict="([^"]*)"/g)) {
    const json = match[1]
      .replaceAll("&quot;", '"')
      .replaceAll("&lt;", "<")
      .replaceAll("&gt;", ">")
      .replaceAll("&amp;", "&");
    let list: unknown;
    try {
      list = JSON.parse(json);
    } catch {
      continue;
    }
    if (!Array.isArray(list)) continue;
    for (const c of list) {
      if (isRecord(c) && c.file === file && typeof c.id === "string") return c.id;
    }
  }
  return null;
}

function suggestionListChangedFor(file: string): (m: WsMessage) => m is SuggestionListChanged {
  return (m: WsMessage): m is SuggestionListChanged =>
    m.type === "suggestion-list-changed" && m.file === file;
}

// ── Socket ──────────────────────────────────────────────────────────────────

/**
 * Open a browser socket. `auth: null` connects anonymously (the public
 * `/_presemble/ws`, ADR-054); otherwise the session cookie is sent on the
 * upgrade request (`/_presemble/editorial-ws`). The `{ headers }` options form
 * is Deno's non-standard WebSocket constructor overload.
 */
function openSocket(url: string, auth: { cookie: string } | null): WebSocket {
  if (auth !== null) {
    return new WebSocket(url, { headers: { cookie: `presemble_session=${auth.cookie}` } });
  }
  return new WebSocket(url);
}

/**
 * Open a socket feeding `inbox`. With `strict`, an unrecognised or non-text
 * frame is recorded as a protocol violation instead of being ignored.
 */
function connect(
  url: string,
  auth: { cookie: string } | null,
  inbox: Inbox,
  strict: boolean,
): WebSocket {
  const ws = openSocket(url, auth);
  ws.onmessage = (ev: MessageEvent) => {
    const data: unknown = ev.data;
    if (typeof data !== "string") {
      if (strict) inbox.protocolViolation("non-text frame");
      else diag(`(${inbox.name}) ignoring non-text frame`);
      return;
    }
    const m = parseWsMessage(data);
    if (m === null) {
      if (strict) inbox.protocolViolation(`unrecognised frame: ${data}`);
      else diag(`(${inbox.name}) unrecognised frame: ${data}`);
      return;
    }
    inbox.push(m);
  };
  ws.addEventListener("close", (ev: CloseEvent) => inbox.close(`code ${ev.code}`));
  return ws;
}

/** PASS `desc` if the open settled successfully, StepFailure otherwise. */
function reportOpen(desc: string, r: PromiseSettledResult<void>): void {
  if (r.status === "rejected") {
    const e: unknown = r.reason;
    throw new StepFailure(desc, e instanceof Error ? e.message : String(e));
  }
  pass(desc);
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

// ── The browser's block-edit builder ────────────────────────────────────────

/** The protocol builders in inject.js, sliced out and evaluated as the browser would. */
interface BlockEditBuilders {
  buildBlockEditRequest(
    file: string,
    selection: string,
    baseMarkdown: string,
    proposedMarkdown: string,
    intent: string,
  ): unknown;
  editTarget(address: string | null, md: string | null): { address: string; md: string } | null;
}

function loadBuilders(): BlockEditBuilders {
  const source = Deno.readTextFileSync(new URL("../resources/serve/inject.js", import.meta.url));
  const start = source.indexOf("// BEGIN protocol-builders");
  const end = source.indexOf("// END protocol-builders");
  if (start < 0 || end < start) {
    throw new StepFailure("ws: inject.js builders", "no protocol-builders region");
  }
  const factory = new Function(
    `${source.slice(start, end)}\nreturn { buildBlockEditRequest: buildBlockEditRequest, editTarget: editTarget };`,
  );
  return factory() as BlockEditBuilders;
}

function unescapeAttr(s: string): string {
  return s
    .replace(/&quot;/g, '"')
    .replace(/&#0?39;|&#x27;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

/**
 * The `index`-th editable element (zero-based) of `file`'s slot `slot` on an
 * editorial page: its emitted `data-presemble-address` and
 * `data-presemble-md`, read through inject.js's `editTarget` as the browser
 * does. An empty slot's placeholder is one (its markdown is empty).
 */
function slotBlockOnPage(
  html: string,
  file: string,
  slot: string,
  index: number,
): { address: string; md: string } | null {
  const attr = (tag: string, name: string): string | null => {
    const m = new RegExp(`\\s${name}="([^"]*)"`).exec(tag);
    return m === null ? null : unescapeAttr(m[1]);
  };
  const { editTarget } = loadBuilders();
  const tags = html.match(/<[a-zA-Z][a-zA-Z0-9]*\s[^>]*data-presemble-address="[^"]*"[^>]*>/g) ?? [];
  const blocks = tags
    .filter((t) => attr(t, "data-presemble-file") === file && attr(t, "data-presemble-slot") === slot)
    .map((t) => editTarget(attr(t, "data-presemble-address"), attr(t, "data-presemble-md")))
    .filter((t): t is { address: string; md: string } => t !== null);
  return blocks[index] ?? null;
}

/** The `index`-th editable block of `file`'s `body` on an editorial page. */
function bodyBlockOnPage(
  html: string,
  file: string,
  index: number,
): { address: string; md: string } | null {
  return slotBlockOnPage(html, file, "body", index);
}

// ── Scenario ────────────────────────────────────────────────────────────────

const FILE = "content/post/hello-world.md";
const PAGE = "/post/hello-world";

const TITLE_SELECTION =
  `(ned/text-in (ned/children (ned/slot (ned/doc-by-path "${FILE}") "title")))`;
const BODY_PARA_SELECTION =
  `(ned/text-containing (ned/body (ned/doc-by-path "${FILE}")) "Smoke body edit via bsave")`;
/** Zero-based body index of a paragraph in FILE; see step 7 in `run`. */
const EDIT_BODY_IDX = 3;

async function run(baseUrl: string, cookie: string, siteDir: string | null): Promise<void> {
  const http = new Http(baseUrl, cookie);
  const wsBase = baseUrl.replace(/^http/, "ws");
  const publicInbox = new Inbox("public");
  const editorialInbox = new Inbox("editorial");

  // 1. Anonymous public live-reload socket + cookie-authenticated editorial
  //    socket. Both opens are awaited together so neither "open" event can
  //    fire before its listener is attached.
  const publicDesc = "ws: public live-reload socket accepts anonymous connection (ADR-054)";
  const editorialDesc = "ws: editorial socket accepts session cookie";
  let publicWs: WebSocket;
  try {
    publicWs = connect(`${wsBase}/_presemble/ws`, null, publicInbox, true);
  } catch (e) {
    throw new StepFailure(publicDesc, String(e));
  }
  let editorialWs: WebSocket;
  try {
    editorialWs = connect(`${wsBase}/_presemble/editorial-ws`, { cookie }, editorialInbox, false);
  } catch (e) {
    publicWs.close();
    throw new StepFailure(editorialDesc, String(e));
  }

  try {
    const [publicOpen, editorialOpen] = await Promise.allSettled([
      waitOpen(publicWs, 5000),
      waitOpen(editorialWs, 5000),
    ]);
    reportOpen(publicDesc, publicOpen);
    reportOpen(editorialDesc, editorialOpen);

    // The public socket also settles longer: the smoketest's earlier saves
    // make the published build announce shortly afterwards.
    await Promise.all([publicInbox.quiesce(1500, 6000), editorialInbox.quiesce()]);

    // 2. A title block edit (built from the editorial page, as the browser
    //    builds it) → reload of the post page.
    {
      const page = await http.get(PAGE);
      const target = slotBlockOnPage(page.text, FILE, "title", 0);
      if (target === null) throw new StepFailure("ws: title block", "not found on the editorial page");
      const request = loadBuilders().buildBlockEditRequest(
        FILE,
        target.address,
        target.md,
        "WS Edit Title",
        "apply",
      );
      const r = await http.post("/_presemble/block-edit", request);
      check(
        "ws: /block-edit on the title returns applied",
        r.status === 200 && r.text.includes('"applied"'),
        r.text,
      );
      const m = await editorialInbox.expect(
        `ws: /block-edit broadcasts editorial reload including ${PAGE}`,
        reloadIncluding(PAGE),
      );
      check(
        `ws: /block-edit editorial reload primary is the edited page ${PAGE}`,
        m.primary === PAGE,
        `primary=${JSON.stringify(m.primary)} pages=${JSON.stringify(m.pages)}`,
      );
      // Readers are served the saved, published page: an edit that is not saved
      // reloads nobody.
      await expectNoPublicMessage("ws: an unsaved /block-edit does not reload readers", publicInbox);
    }

    // 3. /apply → reload of the post page.
    {
      const program =
        `(ned/set-text (ned/text-in (ned/nth-child (ned/slot (ned/doc-by-path "${FILE}") "summary") 1)) ` +
        `"WS APPLY SECOND")`;
      const r = await http.post("/_presemble/apply", { program });
      expectOk("ws: /apply returns ok", r);
      const dirty = isRecord(r.json) ? r.json.dirtyPaths : undefined;
      check("ws: /apply dirties one doc", dirty === 1, `dirtyPaths=${JSON.stringify(dirty)}: ${r.text}`);
      await editorialInbox.expect(
        `ws: /apply broadcasts editorial reload including ${PAGE}`,
        reloadIncluding(PAGE),
      );
      await expectNoPublicMessage("ws: an unsaved /apply does not reload readers", publicInbox);
    }

    // 3b. A file that changes on disk while it has unsaved edits (step 2 and 3
    //     left FILE dirty) in a block those edits changed too (the title) is a
    //     conflict on that block: `conflict` with state `blocks` on the
    //     editorial socket only (the leak check at the end covers the public
    //     one). A change to another block would be merged (ADR-065 amendment
    //     F2). Saving needs consent that names the file.
    if (siteDir !== null) {
      const path = `${siteDir}/${FILE}`;
      const onDisk = await Deno.readTextFile(path);
      await Deno.writeTextFile(path, onDisk.replace(/^# .*$/m, "# Changed From Outside"));
      const conflict = await editorialInbox.expect(
        `ws: an external change to a dirty file broadcasts conflict for ${FILE}`,
        conflictFor(FILE),
      );
      check(
        "ws: the conflict is on blocks",
        conflict.state === "blocks",
        `state=${JSON.stringify(conflict.state)}`,
      );
      await editorialInbox.expect(
        `ws: the conflict broadcasts save-state conflict for ${FILE}`,
        saveStateFor(FILE, "conflict"),
      );
      const refused = await http.post("/_presemble/save-all", {});
      check(
        "ws: save-all with a conflict needs confirmation",
        isRecord(refused.json) && refused.json.needsConfirmation === true,
        refused.text,
      );
      const listed = await http.get("/_presemble/conflicts");
      check(
        "ws: the conflict is listed",
        Array.isArray(listed.json) && listed.json.includes(FILE),
        listed.text,
      );

      // 3b'. The editorial page marks the block; resolving it through the
      //      route ends the conflict, and the end reaches the editors.
      const page = await http.get(PAGE);
      const id = conflictIdOnPage(page.text, FILE);
      if (id === null) {
        throw new StepFailure("ws: the editorial page marks the conflicted block", "no marker");
      }
      pass("ws: the editorial page marks the conflicted block");
      const resolved = await http.post("/_presemble/resolve-conflict", {
        file: FILE,
        id,
        take: "unsaved",
      });
      expectOk("ws: the resolution route resolves the block", resolved);
      await editorialInbox.expect(
        `ws: resolving the last block broadcasts conflict-resolved for ${FILE}`,
        conflictResolvedFor(FILE),
      );
      const after = await http.get("/_presemble/conflicts");
      check(
        "ws: the resolved conflict is no longer listed",
        Array.isArray(after.json) && after.json.length === 0,
        after.text,
      );
      const saved = await http.post("/_presemble/save-all", {});
      expectOk("ws: save-all after the resolution returns ok", saved);
    } else {
      const saved = await http.post("/_presemble/save-all", {});
      expectOk("ws: save-all returns ok", saved);
    }
    // The save reaches the editors as the document's save state.
    {
      const m = await editorialInbox.expect(
        `ws: saving broadcasts save-state saved for ${FILE}`,
        saveStateFor(FILE, "saved"),
      );
      const now = Date.now();
      check(
        "ws: the saved state carries when, in ms since the epoch",
        m.at !== null && m.at > now - 600_000 && m.at <= now + 60_000,
        `at=${m.at} now=${now}`,
      );
    }

    // 3c. The save is what reaches the readers: the published output is rebuilt
    //     from the files on disk and the public socket announces the page.
    await publicInbox.expect(
      `ws: saving broadcasts public reload including ${PAGE}`,
      reloadIncluding(PAGE),
      15000,
    );
    // Settle: the published build may announce more than once.
    await publicInbox.quiesce(1500, 6000);

    // 4. Create + accept (re-renders) → suggestion-list-changed, then reload.
    {
      const id = await createSuggestion(http, "ws: create accept-suggestion returns ok", {
        file: FILE,
        selection: TITLE_SELECTION,
        mutation: { SetText: "WS Accepted Title" },
        reason: "ws smoke accept",
      });
      await editorialInbox.expect(
        "ws: create broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );
      const r = await http.post("/_presemble/ned-suggestions/accept", { id });
      expectOk("ws: accept returns ok", r);
      const m = await editorialInbox.expect(
        `ws: accept broadcasts editorial reload including ${PAGE}`,
        reloadIncluding(PAGE),
      );
      check("ws: accept reload has non-empty pages", m.pages.length > 0, JSON.stringify(m));
      await expectNoPublicMessage("ws: an unsaved accept does not reload readers", publicInbox);
    }

    // 5. Create + reject → suggestion-list-changed twice.
    {
      const id = await createSuggestion(http, "ws: create reject-suggestion returns ok", {
        file: FILE,
        selection: TITLE_SELECTION,
        mutation: { SetText: "WS Rejected Title" },
        reason: "ws smoke reject",
      });
      await editorialInbox.expect(
        "ws: create (reject case) broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );
      const r = await http.post("/_presemble/ned-suggestions/reject", { id });
      expectOk("ws: reject returns ok", r);
      await editorialInbox.expect(
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
      await editorialInbox.expect(
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
      await editorialInbox.expect(
        "ws: create T broadcasts suggestion-list-changed for file",
        suggestionListChangedFor(FILE),
      );

      const rt = await http.post("/_presemble/ned-suggestions/accept", { id: tId });
      expectOk("ws: accept T returns ok", rt);
      await editorialInbox.expect(
        `ws: accept T broadcasts editorial reload including ${PAGE}`,
        reloadIncluding(PAGE),
      );

      const rs = await http.post("/_presemble/ned-suggestions/accept", { id: sId });
      expectOk("ws: accept S (goes stale) still returns ok", rs);
      await editorialInbox.expect(
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

    // 7. A BlockEdit Apply on body element 3 → anchored editorial reload +
    //    plain public reload (ADR-046: the anchor comes from BlockEdit Apply).
    //    Last on purpose: it replaces a body element, which must not disturb
    //    the selections the steps above rely on.
    //    Body of the scaffolded hello-world.md (example-sites/blog), after the
    //    `----` separator, zero-based:
    //      0 "## Getting Started"   1 paragraph "Open the ..."
    //      2 "## What to Edit"      3 paragraph (bsave'd by smoketest.sh D,
    //                                  rewritten by T in step 6 above)
    //    None of the earlier edits change the body's structure, so index 3 is
    //    still a paragraph. The request is built as the browser builds it: from
    //    the editorial page's emitted data-presemble-address and
    //    data-presemble-md, by inject.js's own buildBlockEditRequest.
    {
      const page = await http.get(PAGE);
      const target = bodyBlockOnPage(page.text, FILE, EDIT_BODY_IDX);
      check(
        "ws: body element 3 is an editable paragraph on the editorial page",
        target !== null && !target.md.startsWith("#"),
        `target=${JSON.stringify(target)}`,
      );
      if (target === null) throw new StepFailure("ws: body element 3", "not found");
      const request = loadBuilders().buildBlockEditRequest(
        FILE,
        target.address,
        target.md,
        "WS block-edit paragraph.",
        "apply",
      );
      const r = await http.post("/_presemble/block-edit", request);
      check(
        "ws: block-edit apply on a body block is applied",
        r.status === 200 && r.text.includes('"applied"'),
        r.text,
      );
      const m = await editorialInbox.expect(
        `ws: block-edit broadcasts anchored editorial reload including ${PAGE}`,
        anchoredReloadIncluding(PAGE),
      );
      const nodeKind = m.anchor === null ? undefined : m.anchor["node-kind"];
      check(
        "ws: block-edit anchor targets a paragraph",
        nodeKind === "paragraph",
        `anchor=${JSON.stringify(m.anchor)}`,
      );
      await expectNoPublicMessage("ws: an unsaved body block-edit does not reload readers", publicInbox);
    }

    // 8. Leak check (ADR-054): nothing editorial ever reached the public
    //    socket. Settle briefly so stragglers are counted too.
    await publicInbox.quiesce(300, 1500);
    {
      const leaked = publicInbox.seen().filter(isEditorialOnly);
      check(
        "ws: public socket carried no editorial messages",
        leaked.length === 0,
        `${leaked.length} editorial message(s): ${JSON.stringify(leaked)}`,
      );
    }
  } finally {
    publicWs.close();
    editorialWs.close();
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
    const site = Deno.env.get("PRESEMBLE_SMOKE_SITE");
    await run(
      base.replace(/\/+$/, ""),
      cookie,
      site === undefined || site === "" ? null : site.replace(/\/+$/, ""),
    );
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
