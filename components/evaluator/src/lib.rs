use forms::Form;

/// Evaluate a form against the conductor's live state.
pub fn eval(form: &Form, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    // First, macroexpand
    let expanded = macros::macroexpand(form.clone());
    eval_expanded(&expanded, conductor)
}

/// Evaluate a string expression (read + macroexpand + eval).
pub fn eval_str(code: &str, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    let form = reader::read(code).map_err(|e| format!("read error: {e}"))?;
    eval(&form, conductor)
}

/// Convert a `Value` to a plain string for comparison / concatenation purposes.
/// Uses `display_text()` for text-bearing values, `edn::value_to_edn` as fallback.
fn value_to_string(v: &template::Value) -> String {
    match v.display_text() {
        Some(s) => s,
        None => edn::value_to_edn(v),
    }
}

fn eval_expanded(form: &Form, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    match form {
        // Literals evaluate to themselves
        Form::Str(s) => Ok(template::Value::Text(s.clone())),
        Form::Integer(n) => Ok(template::Value::Text(n.to_string())),
        Form::Bool(b) => Ok(template::Value::Text(b.to_string())),
        Form::Nil => Ok(template::Value::Absent),

        // Keywords evaluate as stem lookups: :post → all items for stem "post"
        Form::Keyword { namespace: None, name } => {
            let items = conductor.query_items_for_stem(name);
            let values: Vec<template::Value> = items
                .into_iter()
                .map(|(url, mut graph)| {
                    graph.insert("url", template::Value::Text(url));
                    template::Value::Record(graph)
                })
                .collect();
            Ok(template::Value::List(values))
        }

        // Namespaced keywords are just data
        Form::Keyword { namespace: Some(_), .. } => Ok(template::Value::Text(form.to_string())),

        // Vectors evaluate each element
        Form::Vector(items) => {
            let values: Vec<template::Value> = items
                .iter()
                .map(|item| eval_expanded(item, conductor))
                .collect::<Result<_, _>>()?;
            Ok(template::Value::List(values))
        }

        // Maps evaluate to records
        Form::Map(pairs) => {
            let mut graph = template::DataGraph::new();
            for (k, v) in pairs {
                let key = match k {
                    Form::Keyword { name, .. } => name.clone(),
                    Form::Str(s) => s.clone(),
                    other => other.to_string(),
                };
                let val = eval_expanded(v, conductor)?;
                graph.insert(&key, val);
            }
            Ok(template::Value::Record(graph))
        }

        // Symbols look up in the environment (not implemented yet — error)
        Form::Symbol(name) => Err(format!("unbound symbol: {name}")),

        // Lists are function calls
        Form::List(items) if !items.is_empty() => {
            let func_form = &items[0];
            let func_name = match func_form {
                Form::Symbol(s) => s.as_str(),
                _ => return Err(format!("expected function name, got: {func_form}")),
            };

            match func_name {
                // Collection operations
                "sort-by" => builtin_sort_by(&items[1..], conductor),
                "take" => builtin_take(&items[1..], conductor),
                "filter" => builtin_filter(&items[1..], conductor),
                "first" => builtin_first(&items[1..], conductor),
                "rest" => builtin_rest(&items[1..], conductor),
                "count" => builtin_count(&items[1..], conductor),
                "reverse" => builtin_reverse(&items[1..], conductor),

                // Data access
                "get-content" => builtin_get_content(&items[1..], conductor),
                "get-schema" => builtin_get_schema(&items[1..], conductor),
                "list-content" => builtin_list_content(conductor),
                "list-schemas" => builtin_list_schemas(conductor),
                "get-in" => builtin_get_in(&items[1..], conductor),

                // Editorial
                "suggest" => builtin_suggest(&items[1..], conductor),
                "get-suggestions" => builtin_get_suggestions(&items[1..], conductor),

                // Arithmetic (useful for REPL)
                "+" => builtin_add(&items[1..], conductor),
                "-" => builtin_sub(&items[1..], conductor),
                "*" => builtin_mul(&items[1..], conductor),

                // Comparison
                "=" => builtin_eq(&items[1..], conductor),

                // String
                "str" => builtin_str(&items[1..], conductor),
                "println" => builtin_println(&items[1..], conductor),

                _ => Err(format!("unknown function: {func_name}")),
            }
        }

        Form::List(_) => Ok(template::Value::Absent), // empty list
        Form::Set(_) => Err("sets not supported in evaluation".into()),
    }
}

// ── Built-in functions ───────────────────────────────────────────────────────

fn builtin_sort_by(
    args: &[Form],
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("sort-by requires at least 2 arguments: collection and field".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    let field = args[1]
        .as_keyword_name()
        .ok_or_else(|| "sort-by field must be a keyword".to_string())?
        .to_string();
    let descending = args.get(2).map(|f| f.is_keyword("desc")).unwrap_or(false);

    let mut items = match collection {
        template::Value::List(items) => items,
        _ => return Err("sort-by expects a list".into()),
    };

    items.sort_by(|a, b| {
        let a_val = if let template::Value::Record(r) = a {
            r.resolve(&[field.as_str()])
                .and_then(|v| v.display_text())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let b_val = if let template::Value::Record(r) = b {
            r.resolve(&[field.as_str()])
                .and_then(|v| v.display_text())
                .unwrap_or_default()
        } else {
            String::new()
        };
        if descending {
            b_val.cmp(&a_val)
        } else {
            a_val.cmp(&b_val)
        }
    });

    Ok(template::Value::List(items))
}

fn builtin_take(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("take requires 2 arguments: collection and count".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    let n_val = eval_expanded(&args[1], cond)?;
    let n: usize = match &n_val {
        template::Value::Text(s) => s
            .parse::<usize>()
            .map_err(|_| format!("take count must be a number, got: {s}"))?,
        _ => return Err("take count must be a number".into()),
    };

    match collection {
        template::Value::List(items) => {
            Ok(template::Value::List(items.into_iter().take(n).collect()))
        }
        _ => Err("take expects a list".into()),
    }
}

fn builtin_filter(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 3 {
        return Err("filter requires 3 arguments: collection, field, value".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    let field = args[1]
        .as_keyword_name()
        .ok_or_else(|| "filter field must be a keyword".to_string())?
        .to_string();
    let target = eval_expanded(&args[2], cond)?;
    let target_str = value_to_string(&target);

    match collection {
        template::Value::List(items) => {
            let filtered = items
                .into_iter()
                .filter(|item| {
                    if let template::Value::Record(r) = item {
                        r.resolve(&[field.as_str()])
                            .and_then(|v| v.display_text())
                            .map(|t| t == target_str)
                            .unwrap_or(false)
                    } else {
                        false
                    }
                })
                .collect();
            Ok(template::Value::List(filtered))
        }
        _ => Err("filter expects a list".into()),
    }
}

fn builtin_first(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("first requires 1 argument".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    match collection {
        template::Value::List(mut items) => Ok(if items.is_empty() {
            template::Value::Absent
        } else {
            items.remove(0)
        }),
        _ => Err("first expects a list".into()),
    }
}

fn builtin_rest(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("rest requires 1 argument".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    match collection {
        template::Value::List(mut items) => {
            if !items.is_empty() {
                items.remove(0);
            }
            Ok(template::Value::List(items))
        }
        _ => Err("rest expects a list".into()),
    }
}

fn builtin_count(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("count requires 1 argument".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    match collection {
        template::Value::List(items) => Ok(template::Value::Text(items.len().to_string())),
        template::Value::Text(s) => Ok(template::Value::Text(s.len().to_string())),
        _ => Err("count expects a list or string".into()),
    }
}

fn builtin_reverse(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("reverse requires 1 argument".into());
    }
    let collection = eval_expanded(&args[0], cond)?;
    match collection {
        template::Value::List(mut items) => {
            items.reverse();
            Ok(template::Value::List(items))
        }
        _ => Err("reverse expects a list".into()),
    }
}

fn builtin_get_content(
    args: &[Form],
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("get-content requires 1 argument".into());
    }
    let path = eval_expanded(&args[0], cond)?;
    let path_str = match &path {
        template::Value::Text(s) => s.clone(),
        _ => return Err("get-content path must be a string".into()),
    };
    let abs_path = cond.site_dir().join(&path_str);
    match cond.document_text(&abs_path) {
        Some(text) => Ok(template::Value::Text(text)),
        None => Err(format!("file not found: {path_str}")),
    }
}

fn builtin_get_schema(
    args: &[Form],
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("get-schema requires 1 argument".into());
    }
    let stem = args[0]
        .as_keyword_name()
        .ok_or_else(|| "get-schema argument must be a keyword".to_string())?;
    match cond.schema_source(stem) {
        Some(src) => Ok(template::Value::Text(src)),
        None => Err(format!("no schema for: {stem}")),
    }
}

fn builtin_list_content(cond: &conductor::Conductor) -> Result<template::Value, String> {
    let graph = cond.site_graph();
    let urls: Vec<template::Value> = graph
        .iter_pages_by_kind(site_index::PageKind::Item)
        .map(|n| template::Value::Text(n.url_path.as_str().to_string()))
        .collect();
    Ok(template::Value::List(urls))
}

fn builtin_list_schemas(cond: &conductor::Conductor) -> Result<template::Value, String> {
    let graph = cond.site_graph();
    let mut stems: Vec<String> = graph
        .iter_pages_by_kind(site_index::PageKind::Item)
        .filter_map(|n| n.page_data().map(|pd| pd.schema_stem.as_str().to_string()))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    stems.sort();
    Ok(template::Value::List(
        stems
            .into_iter()
            .map(template::Value::Text)
            .collect(),
    ))
}

fn builtin_get_in(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("get-in requires at least 2 arguments: value and key(s)".into());
    }
    let value = eval_expanded(&args[0], cond)?;
    let mut current = value;
    for key_form in &args[1..] {
        let key = key_form
            .as_keyword_name()
            .ok_or_else(|| "get-in keys must be keywords".to_string())?;
        current = match current {
            template::Value::Record(ref r) => {
                r.resolve(&[key]).cloned().unwrap_or(template::Value::Absent)
            }
            _ => template::Value::Absent,
        };
    }
    Ok(current)
}

fn builtin_suggest(
    args: &[Form],
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    // (suggest "file" "slot" "value" "reason")
    if args.len() < 4 {
        return Err("suggest requires 4 arguments: file, slot, value, reason".into());
    }
    let file = eval_expanded(&args[0], cond)?;
    let slot = eval_expanded(&args[1], cond)?;
    let value = eval_expanded(&args[2], cond)?;
    let reason = eval_expanded(&args[3], cond)?;

    let file_str = match &file {
        template::Value::Text(s) => s.clone(),
        _ => return Err("suggest: file must be a string".into()),
    };
    let slot_str = match &slot {
        template::Value::Text(s) => s.clone(),
        _ => return Err("suggest: slot must be a string".into()),
    };
    let value_str = match &value {
        template::Value::Text(s) => s.clone(),
        _ => return Err("suggest: value must be a string".into()),
    };
    let reason_str = match &reason {
        template::Value::Text(s) => s.clone(),
        _ => return Err("suggest: reason must be a string".into()),
    };

    match cond
        .handle_command(conductor::Command::SuggestSlotValue {
            file: editorial_types::ContentPath::new(file_str),
            slot: editorial_types::SlotName::new(slot_str),
            value: value_str,
            reason: reason_str,
            author: editorial_types::Author::Tool("repl".to_string()),
        })
        .response
    {
        conductor::Response::SuggestionCreated(id) => {
            Ok(template::Value::Text(id.to_string()))
        }
        conductor::Response::Error(e) => Err(e),
        _ => Err("unexpected response from suggest".into()),
    }
}

fn builtin_get_suggestions(
    args: &[Form],
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("get-suggestions requires 1 argument: file".into());
    }
    let file = eval_expanded(&args[0], cond)?;
    let file_str = match &file {
        template::Value::Text(s) => s.clone(),
        _ => return Err("get-suggestions: file must be a string".into()),
    };

    match cond
        .handle_command(conductor::Command::GetSuggestions {
            file: editorial_types::ContentPath::new(file_str),
        })
        .response
    {
        conductor::Response::Suggestions(suggestions) => {
            let values: Vec<template::Value> = suggestions
                .iter()
                .map(|s| {
                    let mut record = template::DataGraph::new();
                    record.insert("id", template::Value::Text(s.id.to_string()));
                    record.insert("author", template::Value::Text(s.author.to_string()));
                    record.insert("reason", template::Value::Text(s.reason.clone()));
                    template::Value::Record(record)
                })
                .collect();
            Ok(template::Value::List(values))
        }
        _ => Err("unexpected response from get-suggestions".into()),
    }
}

// ── Arithmetic ───────────────────────────────────────────────────────────────

fn builtin_add(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    let mut sum: i64 = 0;
    for arg in args {
        let val = eval_expanded(arg, cond)?;
        let n: i64 = match &val {
            template::Value::Text(s) => s
                .parse()
                .map_err(|_| format!("not a number: {s}"))?,
            _ => return Err("+ expects numbers".into()),
        };
        sum += n;
    }
    Ok(template::Value::Text(sum.to_string()))
}

fn builtin_sub(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("- requires at least 1 argument".into());
    }
    let first = eval_expanded(&args[0], cond)?;
    let mut result: i64 = match &first {
        template::Value::Text(s) => s
            .parse()
            .map_err(|_| format!("not a number: {s}"))?,
        _ => return Err("- expects numbers".into()),
    };
    if args.len() == 1 {
        return Ok(template::Value::Text((-result).to_string()));
    }
    for arg in &args[1..] {
        let val = eval_expanded(arg, cond)?;
        let n: i64 = match &val {
            template::Value::Text(s) => s
                .parse()
                .map_err(|_| format!("not a number: {s}"))?,
            _ => return Err("- expects numbers".into()),
        };
        result -= n;
    }
    Ok(template::Value::Text(result.to_string()))
}

fn builtin_mul(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    let mut product: i64 = 1;
    for arg in args {
        let val = eval_expanded(arg, cond)?;
        let n: i64 = match &val {
            template::Value::Text(s) => s
                .parse()
                .map_err(|_| format!("not a number: {s}"))?,
            _ => return Err("* expects numbers".into()),
        };
        product *= n;
    }
    Ok(template::Value::Text(product.to_string()))
}

fn builtin_eq(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("= requires at least 2 arguments".into());
    }
    let first = eval_expanded(&args[0], cond)?;
    let first_str = value_to_string(&first);
    for arg in &args[1..] {
        let val = eval_expanded(arg, cond)?;
        if first_str != value_to_string(&val) {
            return Ok(template::Value::Text("false".into()));
        }
    }
    Ok(template::Value::Text("true".into()))
}

fn builtin_str(args: &[Form], cond: &conductor::Conductor) -> Result<template::Value, String> {
    let mut result = String::new();
    for arg in args {
        let val = eval_expanded(arg, cond)?;
        result.push_str(&value_to_string(&val));
    }
    Ok(template::Value::Text(result))
}

fn builtin_println(
    args: &[Form],
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    let mut parts = Vec::new();
    for arg in args {
        let val = eval_expanded(arg, cond)?;
        parts.push(value_to_string(&val));
    }
    eprintln!("{}", parts.join(" "));
    Ok(template::Value::Absent)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const POST_SCHEMA_SRC: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";

    /// Build a conductor with no content using a pre-loaded schema.
    fn empty_conductor() -> conductor::Conductor {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .build();
        conductor::Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap()
    }

    /// Build a conductor backed by a temp dir with two post content files.
    fn two_post_conductor() -> (tempfile::TempDir, conductor::Conductor) {
        let dir = tempfile::tempdir().unwrap();

        let schema_dir = dir.path().join("schemas/post");
        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::write(schema_dir.join("item.md"), POST_SCHEMA_SRC).unwrap();

        let tpl_dir = dir.path().join("templates/post");
        std::fs::create_dir_all(&tpl_dir).unwrap();
        std::fs::write(
            tpl_dir.join("item.hiccup"),
            "[:html [:body [:h1 (get input :title)]]]",
        )
        .unwrap();

        let content_dir = dir.path().join("content/post");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(
            content_dir.join("alpha.md"),
            "# Alpha Post\n\n----\n\nBody of alpha.\n",
        )
        .unwrap();
        std::fs::write(
            content_dir.join("beta.md"),
            "# Beta Post\n\n----\n\nBody of beta.\n",
        )
        .unwrap();

        let repo = site_repository::SiteRepository::builder()
            .from_dir(dir.path())
            .build();
        let conductor =
            conductor::Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap();
        (dir, conductor)
    }

    // ── arithmetic ──────────────────────────────────────────────────────────

    #[test]
    fn add_two_numbers() {
        let cond = empty_conductor();
        let result = eval_str("(+ 1 2)", &cond).unwrap();
        assert!(
            matches!(&result, template::Value::Text(s) if s == "3"),
            "expected Text(\"3\"), got {result:?}"
        );
    }

    #[test]
    fn subtract_two_numbers() {
        let cond = empty_conductor();
        let result = eval_str("(- 10 3)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "7"));
    }

    #[test]
    fn multiply_two_numbers() {
        let cond = empty_conductor();
        let result = eval_str("(* 4 5)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "20"));
    }

    #[test]
    fn negate_single_number() {
        let cond = empty_conductor();
        let result = eval_str("(- 5)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "-5"));
    }

    #[test]
    fn equality_true() {
        let cond = empty_conductor();
        let result = eval_str("(= 1 1)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "true"));
    }

    #[test]
    fn equality_false() {
        let cond = empty_conductor();
        let result = eval_str("(= 1 2)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "false"));
    }

    #[test]
    fn str_concatenation() {
        let cond = empty_conductor();
        let result = eval_str(r#"(str "hello" " " "world")"#, &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "hello world"));
    }

    // ── literals ─────────────────────────────────────────────────────────────

    #[test]
    fn string_literal_evaluates_to_text() {
        let cond = empty_conductor();
        let result = eval_str(r#""hello""#, &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "hello"));
    }

    #[test]
    fn integer_literal_evaluates_to_text() {
        let cond = empty_conductor();
        let result = eval_str("42", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "42"));
    }

    #[test]
    fn nil_evaluates_to_absent() {
        let cond = empty_conductor();
        let result = eval_str("nil", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn empty_list_evaluates_to_absent() {
        let cond = empty_conductor();
        let result = eval_str("()", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── threading macros ─────────────────────────────────────────────────────

    #[test]
    fn thread_first_arithmetic() {
        // (-> 1 (+ 2) (* 3)) → (* (+ 1 2) 3) → 9
        let cond = empty_conductor();
        let result = eval_str("(-> 1 (+ 2) (* 3))", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "9"));
    }

    #[test]
    fn thread_last_take() {
        // (->> [1 2 3] (take 2)) → list of 2
        let cond = empty_conductor();
        let result = eval_str("(->> [1 2 3] (take 2))", &cond).unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    // ── collection operations ────────────────────────────────────────────────

    #[test]
    fn count_vector() {
        let cond = empty_conductor();
        let result = eval_str("(count [1 2 3])", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "3"));
    }

    #[test]
    fn first_vector() {
        let cond = empty_conductor();
        let result = eval_str("(first [1 2 3])", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "1"));
    }

    #[test]
    fn rest_vector() {
        let cond = empty_conductor();
        let result = eval_str("(rest [1 2 3])", &cond).unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    #[test]
    fn reverse_vector() {
        let cond = empty_conductor();
        let result = eval_str("(reverse [1 2 3])", &cond).unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 3);
            assert!(matches!(&items[0], template::Value::Text(s) if s == "3"));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn take_from_vector() {
        let cond = empty_conductor();
        let result = eval_str("(take [1 2 3 4 5] 3)", &cond).unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 3));
    }

    // ── conductor-backed queries ──────────────────────────────────────────────

    #[test]
    fn keyword_returns_list_of_records() {
        let (_dir, cond) = two_post_conductor();
        let result = eval_str(":post", &cond).unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    #[test]
    fn list_schemas_returns_list() {
        let (_dir, cond) = two_post_conductor();
        let result = eval_str("(list-schemas)", &cond).unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 1);
            assert!(matches!(&items[0], template::Value::Text(s) if s == "post"));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn get_schema_returns_text() {
        let cond = empty_conductor();
        let result = eval_str("(get-schema :post)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s.contains("Post title")));
    }

    #[test]
    fn unknown_function_returns_error() {
        let cond = empty_conductor();
        let result = eval_str("(frobnicate 1 2)", &cond);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown function"));
    }

    #[test]
    fn unbound_symbol_returns_error() {
        let cond = empty_conductor();
        let result = eval_str("undefined-sym", &cond);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unbound symbol"));
    }

    #[test]
    fn set_form_returns_error() {
        let cond = empty_conductor();
        let result = eval_str("#{1 2}", &cond);
        assert!(result.is_err());
    }
}
