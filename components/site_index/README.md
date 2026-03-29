# site_index

Site directory layout conventions for Presemble.

Encapsulates knowledge of the site directory structure: where schemas, content, and templates live, and how they relate to each other.

## Responsibilities

- Classify any file path into its role (Content, Template, Schema, Unknown)
- Resolve schema, content, and template paths from a schema stem
- Discover all schema stems in a site
- List content files for a given schema stem
- Find the template for a given schema stem
- List all dependents of a schema (schema file + content files + template)
- Load and parse a grammar for a given schema stem

## Used by

`publisher_cli`, `lsp_service`, `lsp_capabilities`

---

[Back to root README](../../README.md)
