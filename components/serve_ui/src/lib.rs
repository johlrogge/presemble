/// Injected CSS for the presemble browser UI.
pub static INJECT_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/serve/inject.css"
));

/// Injected JavaScript for the presemble browser UI.
pub static INJECT_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/serve/inject.js"
));

/// Welcome page shown when the site directory has no built output.
pub static WELCOME_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/serve/welcome.html"
));

/// Reader-only live-reload script: connects to the public `/_presemble/ws`
/// socket and reloads the current page. Contains no editor code.
pub static RELOAD_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../resources/serve/reload.js"
));

/// Who a served page is for, which decides what gets injected into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    /// An authenticated editor: the full editorial UI (CSS + `inject.js`).
    Editor,
    /// An anonymous reader: only the reader live-reload script.
    Reader,
}

/// Build the injection HTML for the given audience.
pub fn build_inject_html(audience: Audience) -> String {
    match audience {
        Audience::Editor => format!("<style>{INJECT_CSS}</style><script>{INJECT_JS}</script>"),
        Audience::Reader => format!("<script>{RELOAD_JS}</script>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_injection_has_no_editor_code() {
        let html = build_inject_html(Audience::Reader);
        assert!(html.contains("/_presemble/ws"));
        assert!(!html.contains("editorial-ws"));
        assert!(!html.contains("dirty-buffers"));
        assert!(!html.contains("<style>"));
    }

    #[test]
    fn editor_injection_uses_editorial_socket() {
        let html = build_inject_html(Audience::Editor);
        assert!(html.contains("/_presemble/editorial-ws"));
        assert!(html.contains("<style>"));
    }
}
