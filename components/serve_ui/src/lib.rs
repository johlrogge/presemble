/// Injected CSS for the presemble browser UI.
pub static INJECT_CSS: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/serve/inject.css"));

/// Injected JavaScript for the presemble browser UI.
pub static INJECT_JS: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../resources/serve/inject.js"));

/// Build the complete injection HTML (style + script tags).
pub fn build_inject_html() -> String {
    format!("<style>{INJECT_CSS}</style><script>{INJECT_JS}</script>")
}
