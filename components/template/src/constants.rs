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
