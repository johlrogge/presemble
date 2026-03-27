use crate::ast::{Expr, Transform};
use crate::error::TemplateError;

/// Parse a full expression (possibly pipe-chained).
pub fn parse_expr(src: &str) -> Result<Expr, TemplateError> {
    // Split on `|` to get pipe stages. We must be careful not to split inside
    // parenthesised argument lists (e.g. `match(a => "x | y")`). A simple depth
    // counter on `(` / `)` is sufficient for our grammar.
    let stages = split_pipe_stages(src);

    if stages.is_empty() {
        return Err(TemplateError::ParseError(format!(
            "empty expression: `{src}`"
        )));
    }

    // The first stage is always either a lookup or a template reference.
    let mut expr = parse_primary(stages[0].trim())?;

    // Each subsequent stage is a transform applied via Pipe.
    for stage in &stages[1..] {
        let transform = parse_transform(stage.trim())?;
        expr = Expr::Pipe(Box::new(expr), transform);
    }

    Ok(expr)
}

/// The first (left-hand) part of an expression: a dot-path lookup or `template:name`.
fn parse_primary(src: &str) -> Result<Expr, TemplateError> {
    if let Some(name) = src.strip_prefix("template:") {
        return Ok(Expr::TemplateRef(name.to_string()));
    }

    if src.is_empty() {
        return Err(TemplateError::ParseError(
            "expected a lookup path but got empty string".to_string(),
        ));
    }

    // A dot-separated path like `article.title` or `article.cover.average_color`.
    let parts: Vec<String> = src.split('.').map(|s| s.to_string()).collect();
    Ok(Expr::Lookup(parts))
}

/// Parse a single pipe stage (everything after one `|`).
fn parse_transform(src: &str) -> Result<Transform, TemplateError> {
    // `template:name` in pipe position
    if let Some(name) = src.strip_prefix("template:") {
        return Ok(Transform::ApplyTemplate(name.to_string()));
    }

    // Bare keyword transforms
    if src == "first" {
        return Ok(Transform::First);
    }
    if src == "rest" {
        return Ok(Transform::Rest);
    }

    // Function-call transforms: name(args...)
    if let Some(open_paren) = src.find('(') {
        let name = src[..open_paren].trim();
        let rest = src[open_paren + 1..].trim();
        let args_str = rest
            .strip_suffix(')')
            .ok_or_else(|| {
                TemplateError::ParseError(format!(
                    "unclosed `(` in transform: `{src}`"
                ))
            })?
            .trim();

        return match name {
            "each" => {
                let template_name = parse_template_ref_arg(args_str, "each")?;
                Ok(Transform::Each(template_name))
            }
            "maybe" => {
                let template_name = parse_template_ref_arg(args_str, "maybe")?;
                Ok(Transform::Maybe(template_name))
            }
            "default" => {
                let value = parse_quoted_string_arg(args_str, "default")?;
                Ok(Transform::Default(value))
            }
            "match" => {
                let arms = parse_match_arms(args_str)?;
                Ok(Transform::Match(arms))
            }
            other => {
                // Generic named transform
                let args = if args_str.is_empty() {
                    vec![]
                } else {
                    parse_generic_args(args_str)
                };
                Ok(Transform::Named(other.to_string(), args))
            }
        };
    }

    // Bare named transform with no arguments
    Ok(Transform::Named(src.to_string(), vec![]))
}

/// Extract a `template:name` argument from inside a function call like `each(template:article_card)`.
fn parse_template_ref_arg(args: &str, fn_name: &str) -> Result<String, TemplateError> {
    let name = args.strip_prefix("template:").ok_or_else(|| {
        TemplateError::ParseError(format!(
            "`{fn_name}` argument must be `template:<name>`, got `{args}`"
        ))
    })?;
    Ok(name.to_string())
}

/// Extract a single double-quoted string argument like `"Untitled"`.
fn parse_quoted_string_arg(args: &str, fn_name: &str) -> Result<String, TemplateError> {
    let inner = args
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(|| {
            TemplateError::ParseError(format!(
                "`{fn_name}` argument must be a quoted string, got `{args}`"
            ))
        })?;
    Ok(inner.to_string())
}

/// Parse `match` arms: `landscape => "cover--landscape", portrait => "cover--portrait"`.
fn parse_match_arms(src: &str) -> Result<Vec<(String, String)>, TemplateError> {
    let mut arms = Vec::new();
    for arm in src.split(',') {
        let arm = arm.trim();
        if arm.is_empty() {
            continue;
        }
        let (lhs, rhs) = arm.split_once("=>").ok_or_else(|| {
            TemplateError::ParseError(format!(
                "expected `key => \"value\"` in match arm, got `{arm}`"
            ))
        })?;
        let key = lhs.trim().to_string();
        let value = rhs.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .ok_or_else(|| {
                TemplateError::ParseError(format!(
                    "match arm value must be a quoted string, got `{value}`"
                ))
            })?
            .to_string();
        arms.push((key, value));
    }
    if arms.is_empty() {
        return Err(TemplateError::ParseError(
            "`match` must have at least one arm".to_string(),
        ));
    }
    Ok(arms)
}

/// Parse a comma-separated list of bare or quoted args.
fn parse_generic_args(src: &str) -> Vec<String> {
    src.split(',')
        .map(|s| {
            let s = s.trim();
            // Strip surrounding quotes if present
            if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                s[1..s.len() - 1].to_string()
            } else {
                s.to_string()
            }
        })
        .collect()
}

/// Split an expression string on `|` (pipe) but only at depth 0 (not inside parentheses).
fn split_pipe_stages(src: &str) -> Vec<&str> {
    let mut stages = Vec::new();
    let mut depth: usize = 0;
    let mut last = 0;
    let bytes = src.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b'|' if depth == 0 => {
                // Accept both bare `|` and ` | ` (with surrounding spaces)
                stages.push(&src[last..i]);
                last = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    stages.push(&src[last..]);
    stages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Expr, Transform};

    #[test]
    fn bare_lookup() {
        let expr = parse_expr("article.title").unwrap();
        assert!(matches!(expr, Expr::Lookup(parts) if parts == vec!["article", "title"]));
    }

    #[test]
    fn maybe_transform() {
        let expr = parse_expr("article.cover | maybe(template:article_cover)").unwrap();
        match expr {
            Expr::Pipe(inner, Transform::Maybe(name)) => {
                assert!(matches!(inner.as_ref(), Expr::Lookup(p) if p == &["article", "cover"]));
                assert_eq!(name, "article_cover");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn each_transform() {
        let expr = parse_expr("articles | each(template:article_card)").unwrap();
        match expr {
            Expr::Pipe(inner, Transform::Each(name)) => {
                assert!(matches!(inner.as_ref(), Expr::Lookup(p) if p == &["articles"]));
                assert_eq!(name, "article_card");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn first_transform() {
        let expr = parse_expr("article.summary | first").unwrap();
        assert!(
            matches!(expr, Expr::Pipe(inner, Transform::First)
                if matches!(inner.as_ref(), Expr::Lookup(p) if p == &["article", "summary"]))
        );
    }

    #[test]
    fn rest_then_each_chained() {
        let expr =
            parse_expr("article.summary | rest | each(template:summary_continuation)").unwrap();
        match expr {
            Expr::Pipe(mid, Transform::Each(name)) => {
                assert_eq!(name, "summary_continuation");
                match mid.as_ref() {
                    Expr::Pipe(inner, Transform::Rest) => {
                        assert!(
                            matches!(inner.as_ref(), Expr::Lookup(p) if p == &["article", "summary"])
                        );
                    }
                    other => panic!("expected Pipe(Lookup, Rest), got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn apply_template_transform() {
        let expr = parse_expr("site | template:header").unwrap();
        match expr {
            Expr::Pipe(inner, Transform::ApplyTemplate(name)) => {
                assert!(matches!(inner.as_ref(), Expr::Lookup(p) if p == &["site"]));
                assert_eq!(name, "header");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn match_transform() {
        let expr = parse_expr(
            r#"orientation | match(landscape => "cover--landscape", portrait => "cover--portrait")"#,
        )
        .unwrap();
        match expr {
            Expr::Pipe(inner, Transform::Match(arms)) => {
                assert!(matches!(inner.as_ref(), Expr::Lookup(p) if p == &["orientation"]));
                assert_eq!(
                    arms,
                    vec![
                        ("landscape".to_string(), "cover--landscape".to_string()),
                        ("portrait".to_string(), "cover--portrait".to_string()),
                    ]
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn default_transform() {
        let expr = parse_expr(r#"subtitle | default("Untitled")"#).unwrap();
        match expr {
            Expr::Pipe(inner, Transform::Default(val)) => {
                assert!(matches!(inner.as_ref(), Expr::Lookup(p) if p == &["subtitle"]));
                assert_eq!(val, "Untitled");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
