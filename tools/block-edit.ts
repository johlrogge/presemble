// block-edit.ts — apply one block edit the way the browser does, for the
// smoketest (tools/smoketest.sh).
//
// Fetches an editorial page, takes the `index`-th element of `file`'s slot
// `slot` that carries a `data-presemble-address` and `data-presemble-md` (an
// empty slot's placeholder is one: its markdown is empty), builds the request
// with inject.js's own `buildBlockEditRequest`, and POSTs it to
// /_presemble/block-edit. Prints the server's response body on stdout, or
// `ERROR: ...` and exits 1 when the page has no such element.
//
//   PRESEMBLE_SMOKE_BASE=http://127.0.0.1:3000 PRESEMBLE_SMOKE_COOKIE=<token> \
//     deno run --no-prompt --allow-net=127.0.0.1:3000 \
//       --allow-env=PRESEMBLE_SMOKE_BASE,PRESEMBLE_SMOKE_COOKIE \
//       --allow-read=resources/serve/inject.js \
//       tools/block-edit.ts <page> <file> <slot> <proposed markdown> [index]
//
// Deliberately import-free, like tools/protocol-fixtures.ts.

interface Builders {
  buildBlockEditRequest(
    file: string,
    selection: string,
    baseMarkdown: string,
    proposedMarkdown: string,
    intent: string,
  ): unknown;
  editTarget(address: string | null, md: string | null): { address: string; md: string } | null;
}

function loadBuilders(): Builders {
  const source = Deno.readTextFileSync(new URL("../resources/serve/inject.js", import.meta.url));
  const start = source.indexOf("// BEGIN protocol-builders");
  const end = source.indexOf("// END protocol-builders");
  if (start < 0 || end < start) throw new Error("inject.js has no protocol-builders region");
  const factory = new Function(
    `${source.slice(start, end)}\nreturn { buildBlockEditRequest: buildBlockEditRequest, editTarget: editTarget };`,
  );
  return factory() as Builders;
}

function unescapeAttr(s: string): string {
  return s
    .replace(/&quot;/g, '"')
    .replace(/&#0?39;|&#x27;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
}

async function main(): Promise<number> {
  const [page, file, slot, proposed, indexArg] = Deno.args;
  const base = Deno.env.get("PRESEMBLE_SMOKE_BASE");
  const cookie = Deno.env.get("PRESEMBLE_SMOKE_COOKIE");
  if (!base || !cookie || !page || !file || !slot || proposed === undefined) {
    console.log("ERROR: usage: block-edit.ts <page> <file> <slot> <proposed> [index]");
    return 1;
  }
  const headers = { cookie: `presemble_session=${cookie}` };
  const html = await (await fetch(`${base}${page}`, { headers })).text();

  const attr = (tag: string, name: string): string | null => {
    const m = new RegExp(`\\s${name}="([^"]*)"`).exec(tag);
    return m === null ? null : unescapeAttr(m[1]);
  };
  const builders = loadBuilders();
  const tags = html.match(/<[a-zA-Z][a-zA-Z0-9]*\s[^>]*data-presemble-address="[^"]*"[^>]*>/g) ?? [];
  const targets = tags
    .filter((t) => attr(t, "data-presemble-file") === file && attr(t, "data-presemble-slot") === slot)
    .map((t) => builders.editTarget(attr(t, "data-presemble-address"), attr(t, "data-presemble-md")))
    .filter((t): t is { address: string; md: string } => t !== null);
  const target = targets[Number(indexArg ?? "0")];
  if (target === undefined) {
    console.log(`ERROR: no editable element for slot ${slot} of ${file} on ${page}`);
    return 1;
  }
  const request = builders.buildBlockEditRequest(file, target.address, target.md, proposed, "apply");
  const response = await fetch(`${base}/_presemble/block-edit`, {
    method: "POST",
    headers: { ...headers, "content-type": "application/json" },
    body: JSON.stringify(request),
  });
  console.log(await response.text());
  return 0;
}

Deno.exit(await main());
