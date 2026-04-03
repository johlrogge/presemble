/// References extracted from a CSS file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StylesheetRefs {
    /// `@import` paths to other stylesheets.
    pub imports: Vec<String>,
    /// `url()` paths to leaf assets (fonts, images, etc.).
    pub asset_urls: Vec<String>,
}

/// Extract all local absolute references from CSS source.
///
/// Separates `@import` stylesheet references from `url()` asset references.
/// Only absolute paths (starting with `/`) are returned. External URLs
/// (containing `://`) and data URIs (starting with `data:`) are excluded.
/// Both lists are deduplicated and sorted.
///
/// # Edge case
/// `@import url("/path.css")` is treated as an import, not an asset URL.
pub fn extract_refs(css: &str) -> StylesheetRefs {
    use cssparser::{Parser, ParserInput};

    let mut imports: Vec<String> = Vec::new();
    let mut asset_urls: Vec<String> = Vec::new();
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    collect_refs(&mut parser, &mut imports, &mut asset_urls);

    imports.sort();
    imports.dedup();
    asset_urls.sort();
    asset_urls.dedup();

    StylesheetRefs {
        imports,
        asset_urls,
    }
}

fn collect_refs(
    parser: &mut cssparser::Parser<'_, '_>,
    imports: &mut Vec<String>,
    asset_urls: &mut Vec<String>,
) {
    use cssparser::Token;

    let mut after_import = false;

    loop {
        let token = match parser.next() {
            Ok(t) => t.clone(),
            Err(_) => break,
        };
        match &token {
            Token::UnquotedUrl(url) => {
                let url = url.to_string();
                if is_local_absolute(&url) {
                    // UnquotedUrl outside url() function — cssparser emits this for
                    // bare url() without quotes. After @import it's a stylesheet ref.
                    if after_import {
                        imports.push(url);
                    } else {
                        asset_urls.push(url);
                    }
                }
                after_import = false;
            }
            Token::QuotedString(s) if after_import => {
                let s = s.to_string();
                if is_local_absolute(&s) {
                    imports.push(s);
                }
                after_import = false;
            }
            Token::AtKeyword(kw) if kw.eq_ignore_ascii_case("import") => {
                after_import = true;
            }
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                // Capture whether this url() appears after @import *before* resetting.
                let is_import_url = after_import;
                after_import = false;
                // url("...") with quotes: cssparser emits Function("url") and the
                // quoted string lives in a nested block.
                let _ = parser.parse_nested_block(|inner| {
                    match inner.next() {
                        Ok(Token::QuotedString(s)) => {
                            let s = s.to_string();
                            if is_local_absolute(&s) {
                                if is_import_url {
                                    imports.push(s);
                                } else {
                                    asset_urls.push(s);
                                }
                            }
                        }
                        Ok(Token::UnquotedUrl(url)) => {
                            // Shouldn't happen (url without quotes inside url()) but
                            // handle it defensively.
                            let url = url.to_string();
                            if is_local_absolute(&url) {
                                if is_import_url {
                                    imports.push(url);
                                } else {
                                    asset_urls.push(url);
                                }
                            }
                        }
                        _ => {}
                    }
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            Token::CurlyBracketBlock | Token::SquareBracketBlock | Token::ParenthesisBlock => {
                after_import = false;
                let _ = parser.parse_nested_block(|inner| {
                    collect_refs(inner, imports, asset_urls);
                    Ok::<(), cssparser::ParseError<'_, ()>>(())
                });
            }
            Token::QuotedString(_) => {
                // A quoted string that's not after @import — ignore.
                // Do NOT reset after_import here; it's already handled by the guard above.
            }
            _ => {
                after_import = false;
            }
        }
    }
}

fn is_local_absolute(path: &str) -> bool {
    path.starts_with('/') && !path.contains("://") && !path.starts_with("data:")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_goes_to_imports() {
        let refs = extract_refs(r#"@import "/reset.css";"#);
        assert_eq!(refs.imports, vec!["/reset.css"]);
        assert!(refs.asset_urls.is_empty(), "unexpected asset_urls: {:?}", refs.asset_urls);
    }

    #[test]
    fn url_goes_to_asset_urls() {
        let refs = extract_refs(r#"div { background: url("/font.woff2"); }"#);
        assert!(refs.imports.is_empty(), "unexpected imports: {:?}", refs.imports);
        assert_eq!(refs.asset_urls, vec!["/font.woff2"]);
    }

    #[test]
    fn import_url_goes_to_imports() {
        let refs = extract_refs(r#"@import url("/reset.css");"#);
        assert_eq!(refs.imports, vec!["/reset.css"]);
        assert!(refs.asset_urls.is_empty(), "unexpected asset_urls: {:?}", refs.asset_urls);
    }

    #[test]
    fn mixed_produces_both() {
        let css = r#"
            @import "/reset.css";
            body { background: url(/assets/bg.png); }
        "#;
        let refs = extract_refs(css);
        assert_eq!(refs.imports, vec!["/reset.css"]);
        assert_eq!(refs.asset_urls, vec!["/assets/bg.png"]);
    }

    #[test]
    fn external_urls_excluded_from_both() {
        let css = r#"
            @import "https://fonts.googleapis.com/css2?family=Roboto";
            body { background: url(https://example.com/img.png); }
        "#;
        let refs = extract_refs(css);
        assert!(refs.imports.is_empty(), "unexpected imports: {:?}", refs.imports);
        assert!(refs.asset_urls.is_empty(), "unexpected asset_urls: {:?}", refs.asset_urls);
    }

    #[test]
    fn data_uris_excluded() {
        let refs = extract_refs("body { background: url(data:image/svg+xml;base64,abc); }");
        assert!(refs.imports.is_empty(), "unexpected imports: {:?}", refs.imports);
        assert!(refs.asset_urls.is_empty(), "unexpected asset_urls: {:?}", refs.asset_urls);
    }

    #[test]
    fn relative_paths_excluded() {
        let css = r#"
            @import "fonts/reset.css";
            body { background: url(images/bg.png); }
        "#;
        let refs = extract_refs(css);
        assert!(refs.imports.is_empty(), "unexpected imports: {:?}", refs.imports);
        assert!(refs.asset_urls.is_empty(), "unexpected asset_urls: {:?}", refs.asset_urls);
    }

    #[test]
    fn deduplication() {
        let css = r#"
            body { background: url(/assets/bg.png); }
            div { background: url(/assets/bg.png); }
        "#;
        let refs = extract_refs(css);
        assert_eq!(refs.asset_urls, vec!["/assets/bg.png"]);
    }

    #[test]
    fn url_in_rule_block() {
        let refs = extract_refs(r#"div { background: url("/bg.png") }"#);
        assert!(refs.imports.is_empty(), "unexpected imports: {:?}", refs.imports);
        assert_eq!(refs.asset_urls, vec!["/bg.png"]);
    }

    // --- Ported from publisher_cli ---

    #[test]
    fn css_url_extracts_absolute_path() {
        let refs = extract_refs("body { background: url(/assets/bg.png); }");
        assert_eq!(refs.asset_urls, vec!["/assets/bg.png"]);
    }

    #[test]
    fn css_url_with_double_quotes() {
        let refs = extract_refs(r#"body { background: url("/assets/font.woff2"); }"#);
        assert_eq!(refs.asset_urls, vec!["/assets/font.woff2"]);
    }

    #[test]
    fn css_url_with_single_quotes() {
        let refs = extract_refs("body { background: url('/assets/icon.svg'); }");
        assert_eq!(refs.asset_urls, vec!["/assets/icon.svg"]);
    }

    #[test]
    fn css_url_ignores_external() {
        let refs = extract_refs("body { background: url(https://example.com/img.png); }");
        assert!(refs.asset_urls.is_empty(), "expected empty but got: {:?}", refs.asset_urls);
    }

    #[test]
    fn css_url_ignores_data_uri() {
        let refs = extract_refs("body { background: url(data:image/svg+xml;base64,abc); }");
        assert!(refs.asset_urls.is_empty(), "expected empty but got: {:?}", refs.asset_urls);
    }

    #[test]
    fn css_url_ignores_relative() {
        let refs = extract_refs("body { background: url(images/bg.png); }");
        assert!(refs.asset_urls.is_empty(), "expected empty but got: {:?}", refs.asset_urls);
    }

    #[test]
    fn css_import_extracts_path() {
        let refs = extract_refs(r#"@import "/assets/reset.css";"#);
        assert_eq!(refs.imports, vec!["/assets/reset.css"]);
    }

    #[test]
    fn css_import_with_url() {
        let refs = extract_refs(r#"@import url("/assets/reset.css");"#);
        assert_eq!(refs.imports, vec!["/assets/reset.css"]);
    }

    #[test]
    fn css_url_deduplicates() {
        let css =
            "body { background: url(/assets/bg.png); } div { background: url(/assets/bg.png); }";
        let refs = extract_refs(css);
        assert_eq!(refs.asset_urls, vec!["/assets/bg.png"]);
    }

    #[test]
    fn css_url_multiple_references() {
        let css = r#"
            @import "/assets/reset.css";
            body { background: url(/assets/bg.png); }
        "#;
        let refs = extract_refs(css);
        assert!(
            refs.imports.contains(&"/assets/reset.css".to_string()),
            "missing reset.css in imports: {:?}",
            refs.imports
        );
        assert!(
            refs.asset_urls.contains(&"/assets/bg.png".to_string()),
            "missing bg.png in asset_urls: {:?}",
            refs.asset_urls
        );
    }
}
