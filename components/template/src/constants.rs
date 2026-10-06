/// Presemble special element names (also used as attribute names for `presemble:define` etc.)
pub const ELEM_INSERT: &str = "presemble:insert";
pub const ELEM_INCLUDE: &str = "presemble:include";
pub const ELEM_APPLY: &str = "presemble:apply";
pub const ELEM_JUXT: &str = "presemble:juxt";
pub const ELEM_DEFINE: &str = "presemble:define";
pub const ELEM_CLASS: &str = "presemble:class";

/// Data graph key for the source file path associated with a page/item.
pub const KEY_PRESEMBLE_FILE: &str = "_presemble_file";

/// Data graph key for the source slot path used in browser editing.
pub const KEY_SOURCE_SLOT: &str = "_source_slot";

/// HTML attribute names emitted by the template renderer for browser editing.
pub const ATTR_SLOT: &str = "data-presemble-slot";
pub const ATTR_FILE: &str = "data-presemble-file";
pub const ATTR_HINT: &str = "data-presemble-hint";
pub const ATTR_MD: &str = "data-presemble-md";
/// The NED selection that resolves to exactly this block (editorial output).
pub const ATTR_ADDRESS: &str = "data-presemble-address";
pub const ATTR_SOURCE_SLOT: &str = "data-presemble-source-slot";
/// The slot's stored text, which the browser editor edits instead of the
/// rendered text (ADR-063). Emitted on heading slots and on synthesized title
/// links.
pub const ATTR_TEXT: &str = "data-presemble-text";

/// Class and message of the placeholder `serve` renders in place of a body
/// HTML block (ADR-062 D2). The placeholder is not a slot.
pub const CLASS_HTML_REMOVED: &str = "presemble-html-removed";
pub const MSG_HTML_REMOVED: &str = "Raw HTML is not published. Remove it to publish this page.";

/// DataGraph key prefix for per-slot schema constraint records.
/// The full key for slot "title" is `"_presemble_schema_constraints_title"`.
/// The value is a `Value::Record` where each entry maps a constraint suffix to its string value.
pub const KEY_SCHEMA_CONSTRAINTS_PREFIX: &str = "_presemble_schema_constraints_";

/// DataGraph key prefix for a heading slot's stored text (ADR-063).
/// The full key for slot "title" is `"_presemble_text_title"`.
/// The value is a `Value::Text` holding the text as stored in the document.
pub const KEY_STORED_TEXT_PREFIX: &str = "_presemble_text_";

/// DataGraph key prefix for a slot's editor attributes (ADR-059 section 10).
/// The full key for slot "title" is `"_presemble_editor_title"`. The value is
/// a `Record` (a single-value slot) or a `List` of records, one per rendered
/// item (a multi-value slot), each holding [`KEY_EDITOR_ADDRESS`] and
/// [`KEY_EDITOR_MD`] as `Text`, either absent when the element has none.
/// Present in editorial graphs only. The address is built without a file (the
/// graph does not know it yet) and filled in at render time.
pub const KEY_EDITOR_PREFIX: &str = "_presemble_editor_";
/// Name of the `Derived` edge from a page's semantic content node to its
/// document root. The underscore keeps it out of slot names, and graph views
/// never read `Derived` edges, so it is not a template path.
pub const KEY_DOCUMENT_EDGE: &str = "_document";
/// Key of the address inside an editor record.
pub const KEY_EDITOR_ADDRESS: &str = "address";
/// Key of the canonical markdown inside an editor record.
pub const KEY_EDITOR_MD: &str = "md";
/// DataGraph key on a synthesized link record holding the stored text of the
/// slot named by `_source_slot`.
pub const KEY_SOURCE_TEXT: &str = "_source_text";

/// HTML attribute names for schema-instance count data (emitted on the document root element
/// during schema-mode rendering so the browser badge overlay can display them).
pub const ATTR_SCHEMA_INSTANCE_COUNT: &str = "data-presemble-schema-instance-count";
pub const ATTR_SCHEMA_INSTANCE_SAMPLE_URL: &str = "data-presemble-schema-instance-sample-url";

/// DataGraph key for the JSON-encoded "included by" list on a schema document.
/// The value is a `Value::Text` containing a JSON array such as
/// `[{"schema":"post","url":"/post/#_schema"}]`.
pub const KEY_SCHEMA_INCLUDED_BY: &str = "_presemble_schema_included_by";

/// HTML attribute name for the "included by" JSON list on the schema document root element.
pub const ATTR_SCHEMA_INCLUDED_BY: &str = "data-presemble-schema-included-by";
