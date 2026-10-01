// ---------------------------------------------------------------------------
// Clojure string literal escaping
// ---------------------------------------------------------------------------
// Duplicated from conductor::conductor — intentional per architect decision.
// Revisit consolidation in Phase C6.

/// Escape a Rust string for use as a Clojure string literal body.
/// Handles `\\` and `\"`. Strips `\r` (CR) and null bytes rather than panicking.
/// Returns the escaped content without surrounding quotes.
fn clj_str_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        match ch {
            '\0' | '\r' => {} // strip CR and null
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// Wrap a Rust string as a Clojure string literal (with surrounding quotes).
pub(crate) fn clj_str(s: &str) -> String {
    format!("\"{}\"", clj_str_escape(s))
}

/// Public version of [`clj_str`]: wrap a Rust `&str` as a Clojure string
/// literal including surrounding double quotes.
///
/// Use this when you need to embed a runtime value inside a Clojure source
/// expression that will be sent to the NED runtime. The helper escapes `\\`
/// and `"`, strips CR and null bytes, and leaves all other characters through.
///
/// Example:
/// ```
/// use editorial_types::clj_str_literal;
/// assert_eq!(clj_str_literal("hello"), "\"hello\"");
/// assert_eq!(clj_str_literal(r#"a"b"#), r#""a\"b""#);
/// ```
pub fn clj_str_literal(s: &str) -> String {
    clj_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clj_str_empty() {
        assert_eq!(clj_str(""), "\"\"");
    }

    #[test]
    fn clj_str_simple() {
        assert_eq!(clj_str("hello"), "\"hello\"");
    }

    #[test]
    fn clj_str_double_quote_escaped() {
        assert_eq!(clj_str("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn clj_str_backslash_escaped() {
        assert_eq!(clj_str("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn clj_str_newline_passthrough() {
        // \n is kept as-is (Clojure allows literal newlines in strings)
        assert_eq!(clj_str("a\nb"), "\"a\nb\"");
    }

    #[test]
    fn clj_str_cr_stripped() {
        assert_eq!(clj_str("a\rb"), "\"ab\"");
    }

    #[test]
    fn clj_str_tab_passthrough() {
        assert_eq!(clj_str("a\tb"), "\"a\tb\"");
    }
}
