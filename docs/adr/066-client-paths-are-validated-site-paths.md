# ADR-066: Client paths are validated site paths

## Status

Accepted

## Context

- The conductor reads and writes files named by its clients: the browser
  (through serve), the editor (LSP, also reachable at /_presemble/lsp), MCP
  tools, the nREPL, the `presemble reload` CLI and the file watcher.
- Before this ADR those names were plain strings, joined onto the site or
  output directory with `Path::join`. An absolute path replaces the base and
  `..` walks out of it. Three holes followed:
  - an authenticated editorial client could write and read any file the
    server process could (DocumentChanged then save; GetDocumentText);
  - the public file server served any file the process could read, without
    login (a raw GET /../…);
  - RenderPage read outside the output directory, and two commands answered
    whether any file existed.
- Fixed in 7c25e5d, 4ecd3ff and 9f44bd2.

## Decision

1. **Every path a client sends is a typed site path, validated when it is
   decoded.**
   - `SitePath`: relative and made only of plain names; refuses empty, NUL,
     absolute and prefixed paths, `.` and `..` segments, and any spelling
     that normalises differently. It is never percent-decoded.
   - `ContentPath`: a `SitePath` under `content/` ending in `.md`.
   - Both deserialize through validation, so no handler can hold an invalid
     path. There is no infallible constructor.
   - Absolute paths from the editor and the watcher become site-relative with
     `SitePath::from_absolute`, which also accepts a site directory reached
     through a symlink.
2. **One resolver turns a site path into a file path:** `resolve_in_site`.
   It resolves symlinks on the deepest part that exists and refuses a result
   outside the site, including through a symlink and through a dangling
   symlink. It is the only way the conductor reads or writes a client-named
   file.
3. **URL paths and schema stems follow the same rule.**
   - Request and render URLs must be plain segments (`plain_url_segments`),
     and the file must resolve inside the output directory (`ensure_within`).
   - A stem from a client must name one of the site's schemas before any path
     is built from it.
4. **No command answers questions about arbitrary files.** ResolveLink and
   ResolveTemplate were removed; a future need goes through `SitePath`.

## Consequences

- The IPC protocol and the LSP adapter carry site-relative paths. All
  clients are in this repository.
- A symlink inside the site that points elsewhere inside the site is
  allowed; the boundary is the site directory.
- Not covered: the site repository reads content by walking the directories
  on disk at start-up, so a symlink placed inside `content/` by someone with
  file system access is read. Hosted mode, where tenants may upload site
  files, must address this (ADR-048).

## Related

ADR-043, ADR-054 (editorial auth boundary), ADR-057, ADR-065.
