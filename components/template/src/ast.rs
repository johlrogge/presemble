/// An expression inside a pipe expression slot.
#[derive(Debug, Clone)]
pub enum Expr {
    /// A dot-separated path lookup: `article.title` → `["article", "title"]`
    Lookup(Vec<String>),
    /// A pipe chain: `expr | transform`
    Pipe(Box<Expr>, Transform),
    /// A bare template reference: `template:header` (used in composition)
    TemplateRef(String),
}

/// A pipe transform.
#[derive(Debug, Clone)]
pub enum Transform {
    /// `each(template:name)` — iterate a collection, apply named template to each item
    Each(String),
    /// `maybe(template:name)` — apply template only if value is present
    Maybe(String),
    /// `template:name` — render value through a named template
    ApplyTemplate(String),
    /// `first` — first element of a collection
    First,
    /// `rest` — all elements except the first
    Rest,
    /// `default("fallback")` — fallback if value is absent
    Default(String),
    /// `match(a => "x", b => "y")` — map enumerated values to strings
    Match(Vec<(String, String)>),
    /// Any other named transform with optional string args: `uppercase`, `date_format("...")`, etc.
    Named(String, Vec<String>),
}
