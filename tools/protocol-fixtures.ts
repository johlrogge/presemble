// protocol-fixtures.ts — check the browser's block-edit protocol builders
// against the shared fixtures in resources/serve/protocol/*.json, which the
// Rust side (editorial_types, conductor) is tested against too (ADR-059 §10).
//
// The builders live in resources/serve/inject.js between the markers
// `// BEGIN protocol-builders` and `// END protocol-builders`. That region is
// sliced out and evaluated on its own; it must not touch the DOM.
//
// Checked, per fixture:
//   request  — buildBlockEditRequest(file, selection, base, proposed, intent,
//              reason) gives exactly the fixture's `request`;
//   response — interpretBlockEditResponse(response) names the outcome the
//              browser acts on: applied, suggestion-created, no-change or
//              refused (with the refusal's message).
//
// Usage (run by tools/smoketest.sh):
//
//   deno run --no-prompt --allow-read=. tools/protocol-fixtures.ts
//
// Fixtures marked `"browser": false` (EDN requests, which only the Rust side
// speaks) are skipped, not checked.
//
// Also checked, per fixture in resources/serve/protocol/editorial/:
//   editorialMessageAction(message) — what the page does with a message of
//   the editorial socket — gives the fixture's `action`. A type the page does
//   not know is ignored, never a reload.
//   reloadDecision(message, pathname) — when the fixture carries `pathname`
//   and `decision`: what the page opened at `pathname` does with the reload
//   (reload in place, navigate to a url, or nothing).
//
// Also checked, per fixture in resources/serve/protocol/public/:
//   readerReloads(message, pathname) — whether a reader's page at `pathname`
//   reloads for a message of the public socket — gives the fixture's
//   `reloads`. It lives in resources/serve/reload.js between
//   `// BEGIN reader-reload` and `// END reader-reload`.
//
// Also checked, per fixture in resources/serve/protocol/page-files/:
//   pageFiles(values) — the documents a page shows, from its
//   [data-presemble-file] values — gives the fixture's `files`, and
//   suggestionListUrl(files) gives its `url` (null: nothing to ask for).
//
// Also checked, per fixture in resources/serve/protocol/editor-submit/:
//   submitOutcomePlan(intent, outcome) — what the block editor does once the
//   server has answered a submit — gives the fixture's `plan`.
//
// Also checked, per path in PAGE_PATH_VECTORS: both copies of the page path
// normaliser (inject.js `_samePagePath`, reload.js `samePagePath`) give the
// same, expected form.
//
// Output: `PASS <fixture>` per fixture, `FAIL <fixture> <what>` per mismatch,
// `SKIP <fixture>` per skipped fixture.
// Exit 0 if everything matches, 1 otherwise.
//
// Deliberately import-free, like tools/text-vectors.ts.

const BEGIN = "// BEGIN protocol-builders";
const END = "// END protocol-builders";

/** The browser only speaks markdown; a fixture in another representation says `"browser": false`. */
type BlockText = { readonly markdown: string } | { readonly edn: string };

interface BlockEditRequest {
  readonly file: string;
  readonly selection: string;
  readonly base: BlockText;
  readonly proposed: BlockText;
  readonly intent: "apply" | "suggest";
  readonly reason?: string;
}

type Outcome =
  | { readonly kind: "applied"; readonly pages: readonly string[] }
  | { readonly kind: "suggestion-created"; readonly id: string }
  | { readonly kind: "no-change" }
  | { readonly kind: "refused"; readonly cause: string; readonly message: string };

interface Builders {
  /** The block edit the link picker sends for the option `text`/`href`. */
  buildLinkPickerRequest(
    file: string,
    selection: string,
    baseMarkdown: string,
    text: string,
    href: string,
  ): BlockEditRequest;
  buildBlockEditRequest(
    file: string,
    selection: string,
    baseMarkdown: string,
    proposedMarkdown: string,
    intent: string,
    reason?: string,
  ): BlockEditRequest;
  interpretBlockEditResponse(response: unknown): Outcome;
  /** What the page does with a message of the editorial socket. */
  editorialMessageAction(message: unknown): string;
  /** What the page opened at `pathname` does with a reload message. */
  reloadDecision(message: unknown, pathname: string): ReloadDecision;
  /** inject.js's `_samePagePath`. */
  samePagePath(path: string): string;
  /** The documents a page shows, from its `[data-presemble-file]` values. */
  pageFiles(values: readonly string[]): string[];
  /** The URL listing the suggestions of `files`; null for none. */
  suggestionListUrl(files: readonly string[]): string | null;
  /** What the block editor does once the server has answered a submit. */
  submitOutcomePlan(intent: "apply" | "suggest", outcomeKind: Outcome["kind"]): SubmitPlan;
}

/** What the block editor does after the server's answer to a submit. */
interface SubmitPlan {
  /** A refusal: the editor stays open with the user's text. */
  readonly keepOpen: boolean;
  /** Put the rendered markup back (inline editor only). */
  readonly restoreMarkup: boolean;
  /** What to refresh after closing. */
  readonly after: "dirty-count" | "after-suggest" | "none";
}

/** An intent, the outcome the server answered with, and the editor's plan. */
interface EditorSubmitFixture {
  readonly intent: "apply" | "suggest";
  readonly outcome: Outcome["kind"];
  readonly plan: SubmitPlan;
}

/** A page's `[data-presemble-file]` values, the documents it shows and the URL of their suggestions. */
interface PageFilesFixture {
  readonly values: readonly string[];
  readonly files: readonly string[];
  readonly url: string | null;
}

type ReloadDecision =
  | { readonly kind: "reload" }
  | { readonly kind: "navigate"; readonly url: string }
  | { readonly kind: "none" };

/** A message of the editorial socket and what the page must do with it. */
interface EditorialFixture {
  readonly message: unknown;
  readonly action: string;
  /** The page's `location.pathname`, for a reload decision. */
  readonly pathname?: string;
  /** What the page at `pathname` does with the reload. */
  readonly decision?: ReloadDecision;
}

interface Fixture {
  readonly request: BlockEditRequest;
  readonly response: unknown;
  /** The link picker option the request was built from, when it was. */
  readonly linkPicker?: { readonly text: string; readonly href: string };
  /** `false` when the browser cannot build this request (for example EDN text). */
  readonly browser?: boolean;
}

/** The markdown of a block text the browser can build; the fixture says so with `browser` unset. */
function markdownOf(text: BlockText): string {
  if (!("markdown" in text)) {
    throw new Error(`a browser fixture must use markdown, got ${JSON.stringify(text)}`);
  }
  return text.markdown;
}

function read(relative: string): string {
  return Deno.readTextFileSync(new URL(relative, import.meta.url));
}

/** Every fixture name in resources/serve/protocol/`sub`, so a new one is checked too. */
function fixtureNames(sub = ""): string[] {
  const dir = new URL(`../resources/serve/protocol/${sub}`, import.meta.url);
  const names: string[] = [];
  for (const entry of Deno.readDirSync(dir)) {
    if (entry.isFile && entry.name.endsWith(".json")) {
      names.push(entry.name.slice(0, -".json".length));
    }
  }
  return names.sort();
}

/** The text between `begin` and `end` in resources/serve/`file`. */
function region(file: string, begin: string, end: string): string {
  const source = read(`../resources/serve/${file}`);
  const start = source.indexOf(begin);
  const stop = source.indexOf(end);
  if (start < 0 || stop < 0 || stop < start) {
    throw new Error(`${file} has no \`${begin}\` … \`${end}\` region`);
  }
  return source.slice(start, stop);
}

/** Whether a reader's page at `pathname` reloads for a message of the public socket. */
type ReaderReloads = (message: unknown, pathname: string) => boolean;

/** A message of the public socket, and whether the reader's page at `pathname` reloads. */
interface PublicFixture {
  readonly message: unknown;
  readonly pathname: string;
  readonly reloads: boolean;
}

/** The one form a page path is compared in (see `PAGE_PATH_VECTORS`). */
type SamePagePath = (path: string) => string;

interface Reader {
  readonly readerReloads: ReaderReloads;
  readonly samePagePath: SamePagePath;
}

function loadReader(): Reader {
  const code = region("reload.js", "// BEGIN reader-reload", "// END reader-reload");
  return new Function(
    `${code}\nreturn { readerReloads: readerReloads, samePagePath: samePagePath };`,
  )() as Reader;
}

function loadBuilders(): Builders {
  const code = region("inject.js", BEGIN, END);
  const factory = new Function(
    `${code}\nreturn { buildBlockEditRequest: buildBlockEditRequest, buildLinkPickerRequest: buildLinkPickerRequest, interpretBlockEditResponse: interpretBlockEditResponse, editorialMessageAction: editorialMessageAction, reloadDecision: reloadDecision, samePagePath: _samePagePath, pageFiles: pageFiles, suggestionListUrl: suggestionListUrl, submitOutcomePlan: submitOutcomePlan };`,
  );
  return factory() as Builders;
}

/**
 * Every path the server answers a page at, and the one form both copies of
 * the normaliser (`_samePagePath` in inject.js, `samePagePath` in reload.js)
 * compare it in. One list for both keeps the twins in step.
 */
const PAGE_PATH_VECTORS: readonly (readonly [string, string])[] = [
  ["/", "/"],
  ["/index.html", "/"],
  ["/post", "/post"],
  ["/post/", "/post"],
  ["/post/index.html", "/post"],
  ["/post/a", "/post/a"],
  ["/post/a/", "/post/a"],
];

function show(value: unknown): string {
  return JSON.stringify(value);
}

/** The outcome a fixture's `response` must read as. */
function expectedOutcome(response: unknown): Outcome {
  const r = response as Record<string, unknown>;
  if (r["refused"] !== undefined) {
    const refused = r["refused"] as { cause: string; message: string };
    return { kind: "refused", cause: refused.cause, message: refused.message };
  }
  const done = r["done"];
  if (done === "no-change") return { kind: "no-change" };
  const d = done as Record<string, Record<string, unknown>>;
  if (d["applied"] !== undefined) {
    return { kind: "applied", pages: d["applied"]["pages"] as string[] };
  }
  return { kind: "suggestion-created", id: d["suggestion-created"]["id"] as string };
}

function main(): number {
  const builders = loadBuilders();
  let failures = 0;
  for (const name of fixtureNames()) {
    const fixture = JSON.parse(read(`../resources/serve/protocol/${name}.json`)) as Fixture;
    if (fixture.browser === false) {
      console.log(`SKIP ${name} (not a browser request)`);
      continue;
    }
    const req = fixture.request;
    // A link picker fixture is built from the picked option; every other
    // fixture from the whole request.
    const built = fixture.linkPicker !== undefined
      ? builders.buildLinkPickerRequest(
        req.file,
        req.selection,
        markdownOf(req.base),
        fixture.linkPicker.text,
        fixture.linkPicker.href,
      )
      : builders.buildBlockEditRequest(
        req.file,
        req.selection,
        markdownOf(req.base),
        markdownOf(req.proposed),
        req.intent,
        req.reason,
      );
    let ok = true;
    if (show(built) !== show(req)) {
      ok = false;
      console.log(`FAIL ${name} request: expected ${show(req)}, got ${show(built)}`);
    }
    const outcome = builders.interpretBlockEditResponse(fixture.response);
    const wanted = expectedOutcome(fixture.response);
    if (show(outcome) !== show(wanted)) {
      ok = false;
      console.log(`FAIL ${name} response: expected ${show(wanted)}, got ${show(outcome)}`);
    }
    if (ok) {
      console.log(`PASS ${name}`);
    } else {
      failures += 1;
    }
  }
  for (const name of fixtureNames("editorial/")) {
    const fixture = JSON.parse(
      read(`../resources/serve/protocol/editorial/${name}.json`),
    ) as EditorialFixture;
    let ok = true;
    const action = builders.editorialMessageAction(fixture.message);
    if (action !== fixture.action) {
      ok = false;
      console.log(
        `FAIL editorial/${name} action: expected ${show(fixture.action)}, got ${show(action)}`,
      );
    }
    if ((fixture.pathname === undefined) !== (fixture.decision === undefined)) {
      ok = false;
      console.log(`FAIL editorial/${name}: \`pathname\` and \`decision\` go together`);
    } else if (fixture.pathname !== undefined) {
      const decision = builders.reloadDecision(fixture.message, fixture.pathname);
      if (show(decision) !== show(fixture.decision)) {
        ok = false;
        console.log(
          `FAIL editorial/${name} decision at ${show(fixture.pathname)}: expected ${
            show(fixture.decision)
          }, got ${show(decision)}`,
        );
      }
    }
    if (ok) {
      console.log(`PASS editorial/${name}`);
    } else {
      failures += 1;
    }
  }
  for (const name of fixtureNames("page-files/")) {
    const fixture = JSON.parse(
      read(`../resources/serve/protocol/page-files/${name}.json`),
    ) as PageFilesFixture;
    let ok = true;
    const files = builders.pageFiles(fixture.values);
    if (show(files) !== show(fixture.files)) {
      ok = false;
      console.log(`FAIL page-files/${name} files: expected ${show(fixture.files)}, got ${show(files)}`);
    }
    const url = builders.suggestionListUrl(fixture.files);
    if (url !== fixture.url) {
      ok = false;
      console.log(`FAIL page-files/${name} url: expected ${show(fixture.url)}, got ${show(url)}`);
    }
    if (ok) {
      console.log(`PASS page-files/${name}`);
    } else {
      failures += 1;
    }
  }
  for (const name of fixtureNames("editor-submit/")) {
    const fixture = JSON.parse(
      read(`../resources/serve/protocol/editor-submit/${name}.json`),
    ) as EditorSubmitFixture;
    const plan = builders.submitOutcomePlan(fixture.intent, fixture.outcome);
    if (show(plan) === show(fixture.plan)) {
      console.log(`PASS editor-submit/${name}`);
    } else {
      failures += 1;
      console.log(
        `FAIL editor-submit/${name} plan: expected ${show(fixture.plan)}, got ${show(plan)}`,
      );
    }
  }
  const reader = loadReader();
  for (const name of fixtureNames("public/")) {
    const fixture = JSON.parse(
      read(`../resources/serve/protocol/public/${name}.json`),
    ) as PublicFixture;
    const reloads = reader.readerReloads(fixture.message, fixture.pathname);
    if (reloads === fixture.reloads) {
      console.log(`PASS public/${name}`);
    } else {
      failures += 1;
      console.log(
        `FAIL public/${name} reloads at ${show(fixture.pathname)}: expected ${
          show(fixture.reloads)
        }, got ${show(reloads)}`,
      );
    }
  }
  const normalisers: readonly (readonly [string, SamePagePath])[] = [
    ["inject.js _samePagePath", builders.samePagePath],
    ["reload.js samePagePath", reader.samePagePath],
  ];
  for (const [path, wanted] of PAGE_PATH_VECTORS) {
    let ok = true;
    for (const [which, normalise] of normalisers) {
      const got = normalise(path);
      if (got !== wanted) {
        ok = false;
        console.log(`FAIL page-path ${show(path)} ${which}: expected ${show(wanted)}, got ${show(got)}`);
      }
    }
    if (ok) {
      console.log(`PASS page-path ${show(path)}`);
    } else {
      failures += 1;
    }
  }
  return failures === 0 ? 0 : 1;
}

Deno.exit(main());
