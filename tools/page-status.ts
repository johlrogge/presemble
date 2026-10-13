// page-status.ts — check what the browser says about the page as a whole: the
// diagnostics panel's sentence and the mascot's face.
//
// Both live in resources/serve/inject.js between the markers
// `// BEGIN page-status` and `// END page-status`. That region is sliced out
// and evaluated on its own; it must not touch the DOM.
//
// Checked:
//   diag-summary — diagSummary(errors, warnings), the diagnostics panel's
//                  sentence ('' when the page has no problems).
//
//   mascot-state — mascotState(mode, suggestions, dirty, errors, warnings),
//                  the mascot's data-status, face and hover title. Errors and
//                  warnings replace only the all-clear: modes, suggestions and
//                  unsaved edits still come first.
//
// Usage (run by tools/smoketest.sh and `just page-status`):
//
//   deno run --allow-read=. tools/page-status.ts
//
// Output: `PASS <group>` per group, `FAIL <group> <input>: expected …, got …`
// per mismatch. Exit 0 if everything matches, 1 otherwise.
//
// Deliberately import-free, like tools/text-vectors.ts.

const BEGIN = "// BEGIN page-status";
const END = "// END page-status";

type Mode = "view" | "edit" | "suggest" | "schema";

type Kind = "edit" | "suggest" | "schema" | "clear" | "unsaved" | "suggestions" | "errors" | "warnings";

interface MascotState {
  readonly kind: Kind;
  readonly face: string;
  readonly title: string;
}

interface PageStatus {
  diagSummary(errors: number, warnings: number): string;
  mascotState(mode: Mode, suggestions: number, dirty: number, errors: number, warnings: number): MascotState;
}

interface SummaryCase {
  readonly errors: number;
  readonly warnings: number;
  readonly summary: string;
}

interface MascotCase {
  readonly mode: Mode;
  readonly suggestions: number;
  readonly dirty: number;
  readonly errors: number;
  readonly warnings: number;
  readonly expected: MascotState;
}

const ONE_ERROR = "1 error on this page. The site can't be published until it's fixed.";
const TWO_ERRORS = "2 errors on this page. The site can't be published until they're fixed.";
const ONE_WARNING = "1 warning on this page.";
const TWO_WARNINGS = "2 warnings on this page.";
const THREE_WARNINGS = "3 warnings on this page.";

const SUMMARY_CASES: readonly SummaryCase[] = [
  { errors: 0, warnings: 0, summary: "" },
  { errors: 1, warnings: 0, summary: ONE_ERROR },
  { errors: 2, warnings: 0, summary: TWO_ERRORS },
  { errors: 0, warnings: 1, summary: ONE_WARNING },
  { errors: 0, warnings: 2, summary: TWO_WARNINGS },
  { errors: 1, warnings: 1, summary: `${ONE_ERROR} ${ONE_WARNING}` },
];

const MASCOT_CASES: readonly MascotCase[] = [
  {
    mode: "view", suggestions: 0, dirty: 0, errors: 0, warnings: 0,
    expected: { kind: "clear", face: "\u{1F44D}", title: "All clear \u{2014} ready to publish" },
  },
  {
    mode: "view", suggestions: 0, dirty: 0, errors: 1, warnings: 0,
    expected: { kind: "errors", face: "\u{1F61F}", title: ONE_ERROR },
  },
  {
    mode: "view", suggestions: 0, dirty: 0, errors: 2, warnings: 3,
    expected: { kind: "errors", face: "\u{1F61F}", title: `${TWO_ERRORS} ${THREE_WARNINGS}` },
  },
  {
    mode: "view", suggestions: 0, dirty: 0, errors: 0, warnings: 1,
    expected: { kind: "warnings", face: "\u{1F914}", title: ONE_WARNING },
  },
  {
    mode: "view", suggestions: 1, dirty: 0, errors: 1, warnings: 0,
    expected: { kind: "suggestions", face: "\u{1F917}", title: "1 suggestion \u{2014} click to edit" },
  },
  {
    mode: "view", suggestions: 0, dirty: 1, errors: 0, warnings: 0,
    expected: { kind: "unsaved", face: "\u{1F4BE}", title: "1 unsaved document \u{2014} click to change" },
  },
  {
    mode: "view", suggestions: 0, dirty: 3, errors: 0, warnings: 0,
    expected: { kind: "unsaved", face: "\u{1F4BE}", title: "3 unsaved documents \u{2014} click to change" },
  },
  {
    mode: "view", suggestions: 2, dirty: 1, errors: 0, warnings: 0,
    expected: { kind: "suggestions", face: "\u{1F917}", title: "2 suggestions (1 unsaved) \u{2014} click to edit" },
  },
  {
    // Unsaved edits outrank the page's diagnostics.
    mode: "view", suggestions: 0, dirty: 1, errors: 1, warnings: 0,
    expected: { kind: "unsaved", face: "\u{1F4BE}", title: "1 unsaved document \u{2014} click to change" },
  },
  {
    mode: "view", suggestions: 1, dirty: 0, errors: 0, warnings: 0,
    expected: { kind: "suggestions", face: "\u{1F917}", title: "1 suggestion \u{2014} click to edit" },
  },
  {
    mode: "edit", suggestions: 0, dirty: 2, errors: 0, warnings: 0,
    expected: { kind: "edit", face: "\u{270F}\u{FE0F}", title: "Edit mode \u{2014} click to change (2 unsaved)" },
  },
  {
    mode: "suggest", suggestions: 0, dirty: 1, errors: 0, warnings: 0,
    expected: { kind: "suggest", face: "\u{1F4AC}", title: "Suggest mode \u{2014} click to change (1 unsaved)" },
  },
  {
    mode: "edit", suggestions: 0, dirty: 0, errors: 1, warnings: 0,
    expected: { kind: "edit", face: "\u{270F}\u{FE0F}", title: "Edit mode \u{2014} click to change" },
  },
  {
    mode: "schema", suggestions: 0, dirty: 0, errors: 1, warnings: 0,
    expected: { kind: "schema", face: "\u{1F4D0}", title: "Structure mode \u{2014} viewing schema" },
  },
];

function read(relative: string): string {
  return Deno.readTextFileSync(new URL(relative, import.meta.url));
}

function loadPageStatus(): PageStatus {
  const source = read("../resources/serve/inject.js");
  const start = source.indexOf(BEGIN);
  const end = source.indexOf(END);
  if (start < 0 || end < 0 || end < start) {
    throw new Error(`inject.js has no \`${BEGIN}\` … \`${END}\` region`);
  }
  const region = source.slice(start, end);
  const factory = new Function(
    `${region}\nreturn { diagSummary: diagSummary, mascotState: mascotState };`,
  );
  return factory() as PageStatus;
}

function show(value: unknown): string {
  return JSON.stringify(value);
}

function report(group: string, total: number, failures: number): void {
  if (failures === 0) {
    console.log(`PASS ${group} (${total} vectors)`);
  }
}

function main(): number {
  const status = loadPageStatus();

  let summaryFailures = 0;
  for (const c of SUMMARY_CASES) {
    const got = status.diagSummary(c.errors, c.warnings);
    if (got !== c.summary) {
      summaryFailures += 1;
      console.log(`FAIL diag-summary (${c.errors},${c.warnings}): expected ${show(c.summary)}, got ${show(got)}`);
    }
  }
  report("diag-summary", SUMMARY_CASES.length, summaryFailures);

  let mascotFailures = 0;
  for (const c of MASCOT_CASES) {
    const got = status.mascotState(c.mode, c.suggestions, c.dirty, c.errors, c.warnings);
    const gotPlain = { kind: got.kind, face: got.face, title: got.title };
    if (show(gotPlain) !== show(c.expected)) {
      mascotFailures += 1;
      const input = `(${c.mode},${c.suggestions},${c.dirty},${c.errors},${c.warnings})`;
      console.log(`FAIL mascot-state ${input}: expected ${show(c.expected)}, got kind ${got.kind} ${show(gotPlain)}`);
    }
  }
  report("mascot-state", MASCOT_CASES.length, mascotFailures);

  return summaryFailures + mascotFailures === 0 ? 0 : 1;
}

Deno.exit(main());
