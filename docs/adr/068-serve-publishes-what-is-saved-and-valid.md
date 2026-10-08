# ADR-068: Serve publishes what is saved and valid; editors see the working copy

## Status

Accepted

## Context

- `presemble serve` serves two audiences (ADR-054): editors, with a session,
  and anonymous readers.
- Until this ADR both were served the same directory, rendered by the
  conductor for editing. Anonymous readers therefore received editor-only
  attributes (each block's markdown and address), pages that fail the
  publish gate (ADR-061), unsaved edits, and build-error pages. ADR-057
  recorded this as open question 2 ("page-body leak").
- Principle: nobody sees what has not been published to them.

## Decision

1. **Two outputs, two writers.**
   - The **editorial output** (`output-editorial/<site>/`) is the working
     copy: rendered by the conductor on every edit, with editor attributes,
     without the publish gate. The conductor never writes anywhere else.
   - The **published output** is what is saved and valid: built from the
     files on disk with the same pipeline and publish gate as
     `presemble build`, without editor attributes. `output/<site>/` is
     written only by `presemble build`. Under `presemble serve`, the served
     version lives in a versioned directory (`<output>.publishing-<n>`) that
     the server swaps atomically: serve keeps the current version as an
     in-memory pointer, each rebuild writes a fresh versioned directory, the
     pointer moves to it in one step, and the previous directory is removed
     once nothing reads it. The published output is rebuilt at start-up,
     after every save and after any external file change. If the site fails
     the gate, the published output is not replaced; readers keep the last
     good version.
2. **Served by audience.** A request with an editor session is served the
   editorial output, build-error pages included. Every other request is
   served the published output. A page that is not in it is a 404, and no
   error page is shown.
3. **Reload by audience.** The public live-reload channel announces changes
   to the published output only. The editorial channel keeps the conductor's
   events.
4. **No exception for local serving.** `presemble serve` without a session
   serves the published output; previewing drafts requires a session
   (`--dev-session`).

## Consequences

- Readers of a running serve see exactly what `presemble build` would
  publish from the files on disk.
- Editors keep the live working copy.
- A save that makes the site fail the gate leaves readers on the last good
  version until it passes again.
- Rebuilding the published output is a full build after each save. If that
  becomes too slow for large sites, make it incremental.

## Related

ADR-054, ADR-057 (open question 2, resolved), ADR-061, ADR-065.
