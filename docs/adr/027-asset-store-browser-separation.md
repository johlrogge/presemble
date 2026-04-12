# ADR-027: Asset store and content browser separation

## Status
Superseded by ADR-034

## Context

Site generators assume all assets live in the repo. Git is terrible for large binary files. Git LFS is painful for large sites. Podcasts, video sites, and photo portfolios have GBs of assets that don't belong in version control.

Assets have two separate concerns: where they're stored (filesystem, S3, CDN) and where they come from (local library, Unsplash, YouTube). Conflating these makes the system rigid.

## Decision

Separate asset management into two interfaces:

**Asset Store** handles storage and URL resolution. Given a logical path, it returns a URL. Given bytes, it stores them and returns a URL. The publisher and templates only deal with URLs — they don't know or care where assets physically reside.

**Content Browser** handles discovery and search. It presents searchable media sources to the author. When the author selects an asset, the browser hands it to the store. The browser doesn't know where things are stored.

The local filesystem store is the default bundled implementation. Remote stores (S3, CDN) are opt-in via site configuration. Content browsers (Unsplash, YouTube) are plugins.

Directory-governed configuration allows different stores for different asset paths.

## Consequences

Binary assets no longer need to live in git. Sites with large media libraries can use remote stores while keeping text assets (CSS, JS) local and version-controlled. The browser editing surface (M5) becomes the natural asset discovery UI. The same browser interface is available to the AI editorial agent (MCP) for suggesting images.
