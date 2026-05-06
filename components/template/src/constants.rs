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
pub const ATTR_SOURCE_SLOT: &str = "data-presemble-source-slot";

/// DataGraph key prefix for per-slot schema constraint records.
/// The full key for slot "title" is `"_presemble_schema_constraints_title"`.
/// The value is a `Value::Record` where each entry maps a constraint suffix to its string value.
pub const KEY_SCHEMA_CONSTRAINTS_PREFIX: &str = "_presemble_schema_constraints_";

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
