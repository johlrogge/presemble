// text-vectors.ts — check the browser's pure text helper against the shared
// vectors in resources/serve/text-vectors.json, which the Rust side
// (content::inline, ned) is tested against too (ADR-063).
//
// The helper lives in resources/serve/inject.js between the markers
// `// BEGIN pure-text-helpers` and `// END pure-text-helpers`. That region is
// sliced out and evaluated on its own; it must not touch the DOM.
//
// Checked:
//   normalize — normalizeAnchorText(input), as content::inline::
//               normalize_plain_text and ned's normalize_ws do. The browser
//               uses it to match scroll anchors (ADR-046).
//
//   word-diff — wordDiff(base, proposed), the word-by-word change a suggestion
//               preview shows, against resources/serve/word-diff-vectors.json
//               (browser-only).
//
// The other groups in text-vectors.json (atx, plain) are Rust-only:
// since the block-edit cutover (ADR-059 §10) the browser no longer derives
// stored text from markdown.
//
// Usage (run by tools/smoketest.sh):
//
//   deno run --allow-read tools/text-vectors.ts
//
// Output: `PASS <group>` per group, `FAIL <group> <input>: expected …, got …`
// per mismatch. Exit 0 if everything matches, 1 otherwise.
//
// Deliberately import-free, like tools/ws-smoke.ts.

const BEGIN = "// BEGIN pure-text-helpers";
const END = "// END pure-text-helpers";

interface Helpers {
  normalizeAnchorText(s: string): string;
  wordDiff(base: string, proposed: string): DiffEntry[];
}

type Pair = readonly [string, unknown];

interface Vectors {
  readonly normalize: readonly Pair[];
}

interface DiffEntry {
  readonly op: "eq" | "del" | "ins";
  readonly text: string;
}

/** A string, or `{repeat, times}` for one too long to write out. */
type VectorText = string | { readonly repeat: string; readonly times: number };

interface WordDiffCase {
  readonly name: string;
  readonly base: VectorText;
  readonly proposed: VectorText;
  readonly diff: readonly { readonly op: DiffEntry["op"]; readonly text: VectorText }[];
}

function text(t: VectorText): string {
  return typeof t === "string" ? t : t.repeat.repeat(t.times);
}

function read(relative: string): string {
  return Deno.readTextFileSync(new URL(relative, import.meta.url));
}

function loadHelpers(): Helpers {
  const source = read("../resources/serve/inject.js");
  const start = source.indexOf(BEGIN);
  const end = source.indexOf(END);
  if (start < 0 || end < 0 || end < start) {
    throw new Error(`inject.js has no \`${BEGIN}\` … \`${END}\` region`);
  }
  const region = source.slice(start, end);
  const factory = new Function(
    `${region}\nreturn { normalizeAnchorText: normalizeAnchorText, wordDiff: wordDiff };`,
  );
  return factory() as Helpers;
}

function show(value: unknown): string {
  return JSON.stringify(value);
}

function check(
  group: string,
  pairs: readonly Pair[],
  run: (input: string) => unknown,
): number {
  let failures = 0;
  for (const [input, expected] of pairs) {
    const got = run(input);
    if (got !== expected) {
      failures += 1;
      console.log(`FAIL ${group} ${show(input)}: expected ${show(expected)}, got ${show(got)}`);
    }
  }
  if (failures === 0) {
    console.log(`PASS ${group} (${pairs.length} vectors)`);
  }
  return failures;
}

function main(): number {
  const helpers = loadHelpers();
  const vectors = JSON.parse(read("../resources/serve/text-vectors.json")) as Vectors;
  let failures = check("normalize", vectors.normalize, (s) => helpers.normalizeAnchorText(s));

  const wordDiff = JSON.parse(read("../resources/serve/word-diff-vectors.json")) as {
    readonly cases: readonly WordDiffCase[];
  };
  let diffFailures = 0;
  for (const c of wordDiff.cases) {
    const expected = c.diff.map((e) => ({ op: e.op, text: text(e.text) }));
    const got = helpers.wordDiff(text(c.base), text(c.proposed));
    if (show(got) !== show(expected)) {
      diffFailures += 1;
      // Long texts are shown by length only, and long diffs by their size.
      const brief = (d: readonly DiffEntry[]) =>
        d.length > 12
          ? `${d.length} entries`
          : show(d.map((e) => (e.text.length > 60 ? { op: e.op, length: e.text.length } : e)));
      console.log(`FAIL word-diff ${c.name}: expected ${brief(expected)}, got ${brief(got)}`);
    }
  }
  if (diffFailures === 0) {
    console.log(`PASS word-diff (${wordDiff.cases.length} vectors)`);
  }
  failures += diffFailures;
  return failures === 0 ? 0 : 1;
}

Deno.exit(main());
