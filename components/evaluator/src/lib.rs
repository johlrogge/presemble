mod env;
mod closure;

pub use env::{Env, RootEnv};
pub use closure::{Closure, FnArity, PrimitiveFn};

use std::sync::Arc;
use forms::Form;

/// Evaluate a form against the conductor's live state.
pub fn eval(form: &Form, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    // First, macroexpand
    let expanded = macros::macroexpand(form.clone());
    let root = RootEnv::new();
    let env = root.snapshot();
    eval_in_env(&expanded, &env, &root, conductor)
}

/// Evaluate a string expression (read + macroexpand + eval).
pub fn eval_str(code: &str, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    let form = reader::read(code).map_err(|e| format!("read error: {e}"))?;
    eval(&form, conductor)
}

/// Internal evaluation entry point — used by closures calling back into the evaluator.
/// Takes an explicit lexical environment and root env in addition to the conductor.
pub(crate) fn eval_in_env(
    form: &Form,
    env: &Arc<Env>,
    root: &RootEnv,
    conductor: &conductor::Conductor,
) -> Result<template::Value, String> {
    match form {
        // Literals evaluate to themselves (ADR-036 Phase 2: proper types)
        Form::Str(s) => Ok(template::Value::Text(s.clone())),
        Form::Integer(n) => Ok(template::Value::Integer(*n)),
        Form::Bool(b) => Ok(template::Value::Bool(*b)),
        Form::Nil => Ok(template::Value::Absent),

        // Unnamespaced keywords: stem query (legacy; Phase 6 migrates to `query` fn)
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

        // Namespaced keywords are first-class values
        Form::Keyword { namespace, name } => Ok(template::Value::Keyword {
            namespace: namespace.clone(),
            name: name.clone(),
        }),

        // Vectors evaluate each element
        Form::Vector(items) => {
            let values: Vec<template::Value> = items
                .iter()
                .map(|item| eval_in_env(item, env, root, conductor))
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
                let val = eval_in_env(v, env, root, conductor)?;
                graph.insert(&key, val);
            }
            Ok(template::Value::Record(graph))
        }

        // Symbols: check lexical env, then root env, then error
        Form::Symbol(name) => {
            env.get(name)
                .or_else(|| root.get(name))
                .ok_or_else(|| format!("unbound symbol: {name}"))
        }

        // Lists: special forms and function calls
        Form::List(items) if !items.is_empty() => {
            let func_form = &items[0];

            // Keywords in function position: (:title record) → (get record :title)
            if let Form::Keyword { namespace: None, name } = func_form {
                if items.len() < 2 {
                    return Err(format!("keyword as function requires an argument: (:{name} map)"));
                }
                let map_val = eval_in_env(&items[1], env, root, conductor)?;
                let default = if items.len() > 2 {
                    eval_in_env(&items[2], env, root, conductor)?
                } else {
                    template::Value::Absent
                };
                return match map_val {
                    template::Value::Record(ref r) => {
                        Ok(r.resolve(&[name]).cloned().unwrap_or(default))
                    }
                    _ => Ok(default),
                };
            }

            // Special forms — checked before evaluating the function position
            if let Some(sym) = func_form.as_symbol() {
                match sym {
                    // ── (if test then [else]) ────────────────────────────────
                    "if" => {
                        if items.len() < 3 {
                            return Err("if requires at least 2 arguments: test and then-branch".into());
                        }
                        let test = eval_in_env(&items[1], env, root, conductor)?;
                        let is_truthy = !matches!(&test, template::Value::Bool(false) | template::Value::Absent);
                        if is_truthy {
                            return eval_in_env(&items[2], env, root, conductor);
                        } else if items.len() > 3 {
                            return eval_in_env(&items[3], env, root, conductor);
                        } else {
                            return Ok(template::Value::Absent);
                        }
                    }

                    // ── (do form...) ─────────────────────────────────────────
                    "do" => {
                        let mut result = template::Value::Absent;
                        for form in &items[1..] {
                            result = eval_in_env(form, env, root, conductor)?;
                        }
                        return Ok(result);
                    }

                    // ── (def name value) ─────────────────────────────────────
                    "def" => {
                        if items.len() < 3 {
                            return Err("def requires 2 arguments: name and value".into());
                        }
                        let name = items[1]
                            .as_symbol()
                            .ok_or("def: first argument must be a symbol")?
                            .to_string();
                        let val = eval_in_env(&items[2], env, root, conductor)?;
                        root.def(name, val.clone());
                        return Ok(val);
                    }

                    // ── (let [x v ...] body...) ──────────────────────────────
                    "let" => {
                        if items.len() < 2 {
                            return Err("let requires a binding vector".into());
                        }
                        let bindings = match &items[1] {
                            Form::Vector(v) => v,
                            _ => return Err("let: first argument must be a binding vector".into()),
                        };
                        if bindings.len() % 2 != 0 {
                            return Err("let: binding vector must have an even number of forms".into());
                        }

                        let mut local = Env::with_parent(env.clone());
                        let chunks: Vec<_> = bindings.chunks(2).collect();
                        for chunk in chunks {
                            let bind_name = chunk[0]
                                .as_symbol()
                                .ok_or("let: binding names must be symbols")?
                                .to_string();
                            let local_arc = Arc::new(local.clone());
                            let val = eval_in_env(&chunk[1], &local_arc, root, conductor)?;
                            local = local.set(bind_name, val);
                        }

                        let local = Arc::new(local);
                        let mut result = template::Value::Absent;
                        for form in &items[2..] {
                            result = eval_in_env(form, &local, root, conductor)?;
                        }
                        return Ok(result);
                    }

                    // ── (fn name? [params...] body...) ───────────────────────
                    // ── (fn name? ([params...] body...) ...) — multi-arity ───
                    "fn" => {
                        return eval_fn_form(&items[1..], env, root);
                    }

                    // ── (quote form) ─────────────────────────────────────────
                    "quote" => {
                        if items.len() < 2 {
                            return Err("quote requires an argument".into());
                        }
                        return form_to_value(&items[1]);
                    }

                    // ── (recur args...) ──────────────────────────────────────
                    // TODO(ADR-036): implement tail-call recur in Phase 3
                    "recur" => {
                        return Err("recur is not yet implemented — use named recursion via def".into());
                    }

                    _ => {} // fall through to function call dispatch
                }
            }

            // ── Function call dispatch ───────────────────────────────────────

            // Evaluate the function position (may be a symbol, lambda, etc.)
            let func_val = eval_in_env(func_form, env, root, conductor);

            // If symbol lookup succeeded and produced a Value::Fn, call it.
            match func_val {
                Ok(template::Value::Fn(ref callable)) => {
                    let args: Vec<template::Value> = items[1..]
                        .iter()
                        .map(|a| eval_in_env(a, env, root, conductor))
                        .collect::<Result<_, _>>()?;

                    // Check if the callable is a Closure (needs conductor for eval).
                    if let Some(closure) = callable.as_any().downcast_ref::<Closure>() {
                        return closure.apply(args, conductor);
                    }
                    // Otherwise, call the primitive directly.
                    return callable.call(args);
                }

                // Symbol not in env/root — fall through to legacy name dispatch.
                Err(_) if func_form.as_symbol().is_some() => {}

                // Evaluation produced a non-fn value — error.
                Ok(other) => {
                    return Err(format!(
                        "not a function: {other:?} (from {func_form})"
                    ));
                }

                // Propagate other errors.
                Err(e) => return Err(e),
            }

            // Legacy name-based dispatch for builtins not yet in root env.
            let func_name = func_form
                .as_symbol()
                .expect("already verified this is a symbol");

            match func_name {
                // Collection operations
                "map" => builtin_map(&items[1..], env, root, conductor),
                "sort-by" => builtin_sort_by(&items[1..], env, root, conductor),
                "take" => builtin_take(&items[1..], env, root, conductor),
                "filter" => builtin_filter(&items[1..], env, root, conductor),
                "first" => builtin_first(&items[1..], env, root, conductor),
                "rest" => builtin_rest(&items[1..], env, root, conductor),
                "count" => builtin_count(&items[1..], env, root, conductor),
                "reverse" => builtin_reverse(&items[1..], env, root, conductor),

                // Data access
                "get" => builtin_get(&items[1..], env, root, conductor),
                "get-in" => builtin_get_in(&items[1..], env, root, conductor),
                "get-content" => builtin_get_content(&items[1..], env, root, conductor),
                "get-schema" => builtin_get_schema(&items[1..], conductor),
                "list-content" => builtin_list_content(conductor),
                "list-schemas" => builtin_list_schemas(conductor),
                "refs-to" => builtin_refs_to(&items[1..], env, root, conductor),
                "refs-from" => builtin_refs_from(&items[1..], env, root, conductor),
                "keys" => builtin_keys(&items[1..], env, root, conductor),
                "vals" => builtin_vals(&items[1..], env, root, conductor),

                // Editorial
                "suggest" => builtin_suggest(&items[1..], env, root, conductor),
                "get-suggestions" => builtin_get_suggestions(&items[1..], env, root, conductor),

                // Arithmetic
                "+" => builtin_add(&items[1..], env, root, conductor),
                "-" => builtin_sub(&items[1..], env, root, conductor),
                "*" => builtin_mul(&items[1..], env, root, conductor),

                // Comparison
                "=" => builtin_eq(&items[1..], env, root, conductor),

                // String
                "str" => builtin_str(&items[1..], env, root, conductor),
                "println" => builtin_println(&items[1..], env, root, conductor),

                // Help
                "doc" => builtin_doc(&items[1..]),

                _ => Err(format!("unknown function: {func_name}")),
            }
        }

        Form::List(_) => Ok(template::Value::Absent), // empty list
        Form::Set(_) => Err("sets not supported in evaluation".into()),
    }
}

// ---------------------------------------------------------------------------
// Special form helpers
// ---------------------------------------------------------------------------

/// Parse and construct a `fn` closure from the remaining items after the `fn` symbol.
fn eval_fn_form(
    items: &[Form],
    env: &Arc<Env>,
    root: &RootEnv,
) -> Result<template::Value, String> {
    if items.is_empty() {
        return Err("fn requires at least a parameter vector".into());
    }

    // Optional name: (fn my-name [params] body)
    let (name, rest) = if let Form::Symbol(s) = &items[0] {
        (Some(s.clone()), &items[1..])
    } else {
        (None, items)
    };

    if rest.is_empty() {
        return Err("fn requires a parameter vector".into());
    }

    let arities = match &rest[0] {
        // Single-arity: (fn [params] body...)
        Form::Vector(_) => {
            let arity = parse_fn_arity(rest)?;
            vec![arity]
        }
        // Multi-arity: (fn ([params] body...) ([params] body...) ...)
        Form::List(_) => {
            rest.iter()
                .map(|clause| {
                    let inner = match clause {
                        Form::List(items) => items.as_slice(),
                        _ => return Err("fn multi-arity clause must be a list".into()),
                    };
                    parse_fn_arity(inner)
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        other => return Err(format!("fn: expected parameter vector, got: {other}")),
    };

    let closure = Closure {
        name,
        arities,
        env: env.clone(),
        root: root.clone(),
    };
    Ok(template::Value::Fn(Arc::new(closure)))
}

/// Parse a single fn arity from `[params] body...`
fn parse_fn_arity(items: &[Form]) -> Result<FnArity, String> {
    let param_vec = match items.first() {
        Some(Form::Vector(v)) => v,
        other => return Err(format!("fn arity must start with a parameter vector, got: {other:?}")),
    };

    let mut params = Vec::new();
    let mut rest_param = None;
    let mut saw_amp = false;

    for p in param_vec {
        match p {
            Form::Symbol(s) if s == "&" => {
                saw_amp = true;
            }
            Form::Symbol(s) if saw_amp => {
                rest_param = Some(s.clone());
            }
            Form::Symbol(s) => {
                params.push(s.clone());
            }
            other => {
                return Err(format!("fn parameter must be a symbol, got: {other}"));
            }
        }
    }

    let body = items[1..].to_vec();
    Ok(FnArity { params, rest_param, body })
}

/// Convert a `Form` to a `Value` for `quote`.
fn form_to_value(form: &Form) -> Result<template::Value, String> {
    match form {
        Form::Str(s) => Ok(template::Value::Text(s.clone())),
        Form::Integer(n) => Ok(template::Value::Integer(*n)),
        Form::Bool(b) => Ok(template::Value::Bool(*b)),
        Form::Nil => Ok(template::Value::Absent),
        Form::Symbol(s) => Ok(template::Value::Text(s.clone())),
        Form::Keyword { namespace, name } => Ok(template::Value::Keyword {
            namespace: namespace.clone(),
            name: name.clone(),
        }),
        Form::Vector(items) => {
            let values: Vec<template::Value> = items
                .iter()
                .map(form_to_value)
                .collect::<Result<_, _>>()?;
            Ok(template::Value::List(values))
        }
        Form::List(items) => {
            let values: Vec<template::Value> = items
                .iter()
                .map(form_to_value)
                .collect::<Result<_, _>>()?;
            Ok(template::Value::List(values))
        }
        Form::Map(pairs) => {
            let mut graph = template::DataGraph::new();
            for (k, v) in pairs {
                let key = match k {
                    Form::Keyword { name, .. } => name.clone(),
                    Form::Str(s) => s.clone(),
                    Form::Symbol(s) => s.clone(),
                    other => other.to_string(),
                };
                graph.insert(&key, form_to_value(v)?);
            }
            Ok(template::Value::Record(graph))
        }
        Form::Set(_) => Err("cannot quote a set".into()),
    }
}

/// Convert a `Value` to a plain string for comparison / concatenation purposes.
fn value_to_string(v: &template::Value) -> String {
    match v.display_text() {
        Some(s) => s,
        None => edn::value_to_edn(v),
    }
}

/// Extract an i64 from a Value for arithmetic operations.
fn value_to_i64(v: &template::Value, op: &str) -> Result<i64, String> {
    match v {
        template::Value::Integer(n) => Ok(*n),
        template::Value::Text(s) => s
            .parse::<i64>()
            .map_err(|_| format!("{op}: not a number: {s}")),
        other => Err(format!("{op}: expected a number, got: {other:?}")),
    }
}

// ── Built-in functions ───────────────────────────────────────────────────────

/// (map f coll) — apply f to each item. f can be a keyword or function name.
/// (map :title posts) → list of titles
/// (->> :post (map :title)) → same with threading
fn builtin_map(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("map requires 2 arguments: function and collection".into());
    }
    let func = &args[0];
    let collection = eval_in_env(args.last().unwrap(), env, root, cond)?;

    let items = match collection {
        template::Value::List(items) => items,
        _ => return Err("map expects a list as the last argument".into()),
    };

    let results: Vec<template::Value> = items
        .into_iter()
        .map(|item| apply_to_value(func, &item, env, root, cond))
        .collect::<Result<_, _>>()?;

    Ok(template::Value::List(results))
}

/// Apply a form (keyword or fn value) to a Value directly.
/// For keywords: (:title record) → get the field.
/// For fn values (closures / primitives): call them with the item.
fn apply_to_value(func: &Form, val: &template::Value, env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    match func {
        Form::Keyword { namespace: None, name } => {
            // Keyword as accessor: (:title record) → get the field
            match val {
                template::Value::Record(r) => {
                    Ok(r.resolve(&[name]).cloned().unwrap_or(template::Value::Absent))
                }
                _ => Ok(template::Value::Absent),
            }
        }
        Form::Symbol(_) | Form::List(_) => {
            // Evaluate the func form to get a callable, then apply it.
            let callable_val = eval_in_env(func, env, root, cond)?;
            match callable_val {
                template::Value::Fn(ref f) => {
                    if let Some(closure) = f.as_any().downcast_ref::<Closure>() {
                        closure.apply(vec![val.clone()], cond)
                    } else {
                        f.call(vec![val.clone()])
                    }
                }
                _ => Err(format!("map: {func} is not a function")),
            }
        }
        _ => Err(format!("map function must be a keyword or symbol, got: {func}")),
    }
}

fn builtin_sort_by(
    args: &[Form],
    env: &Arc<Env>,
    root: &RootEnv,
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    // (sort-by :field coll) or (sort-by :field :desc coll)
    // Collection is always the LAST argument (works with ->> threading)
    if args.len() < 2 {
        return Err("sort-by requires at least 2 arguments: field and collection".into());
    }
    let collection = eval_in_env(args.last().unwrap(), env, root, cond)?;
    let field = args[0]
        .as_keyword_name()
        .ok_or_else(|| "sort-by field must be a keyword".to_string())?
        .to_string();
    let descending = args.len() > 2 && args[1].is_keyword("desc");

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

fn builtin_take(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("take requires 2 arguments: count and collection (Clojure convention)".into());
    }
    // Clojure convention: (take n coll) — count first, collection second
    let n_val = eval_in_env(&args[0], env, root, cond)?;
    let n: usize = match &n_val {
        template::Value::Integer(n) => *n as usize,
        template::Value::Text(s) => s
            .parse::<usize>()
            .map_err(|_| format!("take count must be a number, got: {s}"))?,
        _ => return Err("take count must be a number".into()),
    };
    let collection = eval_in_env(&args[1], env, root, cond)?;

    match collection {
        template::Value::List(items) => {
            Ok(template::Value::List(items.into_iter().take(n).collect()))
        }
        _ => Err("take expects a list".into()),
    }
}

fn builtin_filter(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    // (filter :field "value" coll) — collection is LAST (works with ->> threading)
    if args.len() < 3 {
        return Err("filter requires 3 arguments: field, value, collection".into());
    }
    let collection = eval_in_env(args.last().unwrap(), env, root, cond)?;
    let field = args[0]
        .as_keyword_name()
        .ok_or_else(|| "filter field must be a keyword".to_string())?
        .to_string();
    let target = eval_in_env(&args[1], env, root, cond)?;
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

fn builtin_first(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("first requires 1 argument".into());
    }
    let collection = eval_in_env(&args[0], env, root, cond)?;
    match collection {
        template::Value::List(mut items) => Ok(if items.is_empty() {
            template::Value::Absent
        } else {
            items.remove(0)
        }),
        _ => Err("first expects a list".into()),
    }
}

fn builtin_rest(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("rest requires 1 argument".into());
    }
    let collection = eval_in_env(&args[0], env, root, cond)?;
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

fn builtin_count(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("count requires 1 argument".into());
    }
    let collection = eval_in_env(&args[0], env, root, cond)?;
    match collection {
        template::Value::List(items) => Ok(template::Value::Integer(items.len() as i64)),
        template::Value::Text(s) => Ok(template::Value::Integer(s.len() as i64)),
        _ => Err("count expects a list or string".into()),
    }
}

fn builtin_reverse(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("reverse requires 1 argument".into());
    }
    let collection = eval_in_env(&args[0], env, root, cond)?;
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
    env: &Arc<Env>,
    root: &RootEnv,
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("get-content requires 1 argument".into());
    }
    let path = eval_in_env(&args[0], env, root, cond)?;
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

/// (refs-to "/author/alice") — returns all edges pointing TO the given URL.
/// Each edge is returned as a record with keys `source` and `target`.
fn builtin_refs_to(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("refs-to requires 1 argument: a URL path string".into());
    }
    let url = eval_in_env(&args[0], env, root, cond)?;
    let url_str = match &url {
        template::Value::Text(s) => s.clone(),
        _ => return Err("refs-to: argument must be a string URL path".into()),
    };
    let edges = cond.query_edges_to(&url_str);
    Ok(template::Value::List(
        edges.iter().map(edge_to_value).collect(),
    ))
}

/// (refs-from "/post/hello") — returns all edges originating FROM the given URL.
/// Each edge is returned as a record with keys `source` and `target`.
fn builtin_refs_from(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("refs-from requires 1 argument: a URL path string".into());
    }
    let url = eval_in_env(&args[0], env, root, cond)?;
    let url_str = match &url {
        template::Value::Text(s) => s.clone(),
        _ => return Err("refs-from: argument must be a string URL path".into()),
    };
    let edges = cond.query_edges_from(&url_str);
    Ok(template::Value::List(
        edges.iter().map(edge_to_value).collect(),
    ))
}

fn edge_to_value(edge: &site_index::Edge) -> template::Value {
    let mut record = template::DataGraph::new();
    record.insert("source", template::Value::Text(edge.source.as_str().to_string()));
    record.insert("target", template::Value::Text(edge.target.as_str().to_string()));
    template::Value::Record(record)
}

/// (get map :key) or (get map :key default)
fn builtin_get(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("get requires at least 2 arguments: map and key".into());
    }
    let value = eval_in_env(&args[0], env, root, cond)?;
    let key = args[1]
        .as_keyword_name()
        .ok_or_else(|| "get key must be a keyword".to_string())?;
    let default = if args.len() > 2 {
        eval_in_env(&args[2], env, root, cond)?
    } else {
        template::Value::Absent
    };
    match value {
        template::Value::Record(ref r) => {
            Ok(r.resolve(&[key]).cloned().unwrap_or(default))
        }
        _ => Ok(default),
    }
}

/// (keys map) — return all keys of a record as a list of strings
fn builtin_keys(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("keys requires 1 argument".into());
    }
    let value = eval_in_env(&args[0], env, root, cond)?;
    match value {
        template::Value::Record(ref r) => {
            let keys: Vec<template::Value> = r
                .iter()
                .filter(|(k, _)| !k.starts_with('_'))
                .map(|(k, _)| template::Value::Text(k.clone()))
                .collect();
            Ok(template::Value::List(keys))
        }
        _ => Err("keys expects a map/record".into()),
    }
}

/// (vals map) — return all values of a record as a list
fn builtin_vals(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("vals requires 1 argument".into());
    }
    let value = eval_in_env(&args[0], env, root, cond)?;
    match value {
        template::Value::Record(ref r) => {
            let vals: Vec<template::Value> = r
                .iter()
                .filter(|(k, _)| !k.starts_with('_'))
                .map(|(_, v)| v.clone())
                .collect();
            Ok(template::Value::List(vals))
        }
        _ => Err("vals expects a map/record".into()),
    }
}

fn builtin_get_in(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("get-in requires at least 2 arguments: value and key(s)".into());
    }
    let value = eval_in_env(&args[0], env, root, cond)?;
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
    env: &Arc<Env>,
    root: &RootEnv,
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    // (suggest "file" "slot" "value" "reason")
    if args.len() < 4 {
        return Err("suggest requires 4 arguments: file, slot, value, reason".into());
    }
    let file = eval_in_env(&args[0], env, root, cond)?;
    let slot = eval_in_env(&args[1], env, root, cond)?;
    let value = eval_in_env(&args[2], env, root, cond)?;
    let reason = eval_in_env(&args[3], env, root, cond)?;

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
    env: &Arc<Env>,
    root: &RootEnv,
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("get-suggestions requires 1 argument: file".into());
    }
    let file = eval_in_env(&args[0], env, root, cond)?;
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

fn builtin_add(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    let mut sum: i64 = 0;
    for arg in args {
        let val = eval_in_env(arg, env, root, cond)?;
        sum += value_to_i64(&val, "+")?;
    }
    Ok(template::Value::Integer(sum))
}

fn builtin_sub(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.is_empty() {
        return Err("- requires at least 1 argument".into());
    }
    let first = eval_in_env(&args[0], env, root, cond)?;
    let mut result: i64 = value_to_i64(&first, "-")?;
    if args.len() == 1 {
        return Ok(template::Value::Integer(-result));
    }
    for arg in &args[1..] {
        let val = eval_in_env(arg, env, root, cond)?;
        result -= value_to_i64(&val, "-")?;
    }
    Ok(template::Value::Integer(result))
}

fn builtin_mul(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    let mut product: i64 = 1;
    for arg in args {
        let val = eval_in_env(arg, env, root, cond)?;
        product *= value_to_i64(&val, "*")?;
    }
    Ok(template::Value::Integer(product))
}

fn builtin_eq(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    if args.len() < 2 {
        return Err("= requires at least 2 arguments".into());
    }
    let first = eval_in_env(&args[0], env, root, cond)?;
    let first_str = value_to_string(&first);
    for arg in &args[1..] {
        let val = eval_in_env(arg, env, root, cond)?;
        if first_str != value_to_string(&val) {
            return Ok(template::Value::Bool(false));
        }
    }
    Ok(template::Value::Bool(true))
}

fn builtin_str(args: &[Form], env: &Arc<Env>, root: &RootEnv, cond: &conductor::Conductor) -> Result<template::Value, String> {
    let mut result = String::new();
    for arg in args {
        let val = eval_in_env(arg, env, root, cond)?;
        result.push_str(&value_to_string(&val));
    }
    Ok(template::Value::Text(result))
}

fn builtin_println(
    args: &[Form],
    env: &Arc<Env>,
    root: &RootEnv,
    cond: &conductor::Conductor,
) -> Result<template::Value, String> {
    let mut parts = Vec::new();
    for arg in args {
        let val = eval_in_env(arg, env, root, cond)?;
        parts.push(value_to_string(&val));
    }
    eprintln!("{}", parts.join(" "));
    Ok(template::Value::Absent)
}

// ── Documentation ───────────────────────────────────────────────────────────

const DOCS: &[(&str, &str, &str)] = &[
    // (name, signature, description)
    // Collections
    ("map", "(map :field coll)", "Apply a keyword accessor to each item. (->> :post (map :title)) → list of titles."),
    ("sort-by", "(sort-by :field coll) or (sort-by :field :desc coll)", "Sort a collection by a field. Optional :desc for descending."),
    ("take", "(take n coll)", "Take the first n items from a collection."),
    ("filter", "(filter :field \"value\" coll)", "Keep items where field matches value."),
    ("first", "(first coll)", "Return the first item of a collection."),
    ("rest", "(rest coll)", "Return all items except the first."),
    ("count", "(count coll)", "Return the number of items in a collection or characters in a string."),
    ("reverse", "(reverse coll)", "Reverse a collection."),
    // Data access
    ("get", "(get map :key) or (get map :key default)", "Get a value from a record by keyword."),
    ("get-in", "(get-in map :key1 :key2 ...)", "Get a nested value from a record."),
    ("keys", "(keys map)", "Return all keys of a record."),
    ("vals", "(vals map)", "Return all values of a record."),
    ("get-content", "(get-content \"content/post/hello.md\")", "Get the live content of a file (includes unsaved editor changes)."),
    ("get-schema", "(get-schema :post)", "Get the schema source for a content type."),
    ("list-content", "(list-content)", "List all content page URLs."),
    ("list-schemas", "(list-schemas)", "List all schema stem names."),
    // Editorial
    ("suggest", "(suggest \"file\" \"slot\" \"value\" \"reason\")", "Create an editorial suggestion."),
    ("get-suggestions", "(get-suggestions \"file\")", "Get pending suggestions for a file."),
    // Graph queries
    ("refs-to", "(refs-to \"/author/alice\")", "Returns all edges pointing to the given URL. Each edge is a map with :source, :target, :slot, :kind."),
    ("refs-from", "(refs-from \"/post/hello\")", "Returns all edges originating from the given URL."),
    // Arithmetic
    ("+", "(+ a b ...)", "Add numbers."),
    ("-", "(- a b ...)", "Subtract numbers. (- a) negates."),
    ("*", "(* a b ...)", "Multiply numbers."),
    // Comparison
    ("=", "(= a b ...)", "Check equality."),
    // String
    ("str", "(str a b ...)", "Concatenate values as strings."),
    ("println", "(println a b ...)", "Print values to stderr."),
    // Help
    ("doc", "(doc) or (doc fn-name)", "Show documentation for a function, or list all functions."),
    // Macros
    ("->", "(-> x (f a) (g b))", "Thread-first: insert x as first argument in each form."),
    ("->>", "(->> x (f a) (g b))", "Thread-last: insert x as last argument in each form."),
    // Keywords
    (":keyword", ":post", "A bare keyword evaluates to all items for that schema stem."),
];

fn builtin_doc(args: &[Form]) -> Result<template::Value, String> {
    if args.is_empty() {
        // List all functions
        let mut help = String::from("Available functions:\n\n");
        for (name, sig, desc) in DOCS {
            help.push_str(&format!("  {name:<16} {desc}\n"));
            help.push_str(&format!("  {:<16} {sig}\n\n", ""));
        }
        return Ok(template::Value::Text(help));
    }
    let name = match &args[0] {
        Form::Symbol(s) => s.as_str(),
        Form::Str(s) => s.as_str(),
        other => return Err(format!("doc expects a symbol or string, got: {other}")),
    };
    for (doc_name, sig, desc) in DOCS {
        if *doc_name == name {
            return Ok(template::Value::Text(format!("{doc_name}\n  {sig}\n  {desc}")));
        }
    }
    Err(format!("no documentation for: {name}"))
}

// ── Legacy string-based REPL evaluator ───────────────────────────────────────
// This is the predecessor to `eval_str` / `eval`. New callers should prefer
// `eval_str`. Kept here for backwards compatibility with existing call sites.

/// Evaluate an expression in the REPL context against the conductor's live state.
/// Supports a limited set of string-based commands (legacy interface).
/// New code should prefer `eval_str` instead.
pub fn eval_repl(code: &str, conductor: &conductor::Conductor) -> Result<template::Value, String> {
    use std::collections::HashSet;
    let code = code.trim();

    if code.is_empty() {
        return Ok(template::Value::Absent);
    }

    // Bare keyword: :stem → all items for that stem
    if code.starts_with(':') && !code.contains(' ') {
        let stem = &code[1..]; // strip leading ':'
        let items = conductor.query_items_for_stem(stem);
        let values: Vec<template::Value> = items
            .into_iter()
            .map(|(url, mut graph)| {
                graph.insert("url", template::Value::Text(url));
                template::Value::Record(graph)
            })
            .collect();
        return Ok(template::Value::List(values));
    }

    // Thread expression: (->> :stem ...) or (-> :stem ...)
    if code.starts_with("(->>") || code.starts_with("(->") {
        let target = content::parse_link_target(code)
            .map_err(|e| format!("parse error: {e}"))?;
        let text = content::LinkText::Empty;
        let (url_index, stem_index) = repl_build_indexes(conductor);
        let edge_index = expressions::EdgeIndex::new();
        let current_url = site_index::UrlPath::new("/");
        return Ok(expressions::evaluate_link_expression(
            &text,
            &target,
            &url_index,
            &stem_index,
            &current_url,
            &edge_index,
        ));
    }

    // (get-content "path")
    if code.starts_with("(get-content") {
        let path = repl_extract_string_arg(code)?;
        let abs_path = conductor.site_dir().join(&path);
        match conductor.document_text(&abs_path) {
            Some(text) => return Ok(template::Value::Text(text)),
            None => return Err(format!("file not found: {path}")),
        }
    }

    // (get-schema :stem)
    if code.starts_with("(get-schema") {
        let stem = repl_extract_keyword_arg(code)?;
        match conductor.schema_source(&stem) {
            Some(src) => return Ok(template::Value::Text(src)),
            None => return Err(format!("no schema for: {stem}")),
        }
    }

    // (list-content)
    if code.starts_with("(list-content") {
        let graph = conductor.site_graph();
        let mut urls: Vec<template::Value> = graph
            .iter_pages_by_kind(site_index::PageKind::Item)
            .map(|n| template::Value::Text(n.url_path.as_str().to_string()))
            .collect();
        urls.sort_by(|a, b| {
            let a = if let template::Value::Text(s) = a { s.as_str() } else { "" };
            let b = if let template::Value::Text(s) = b { s.as_str() } else { "" };
            a.cmp(b)
        });
        return Ok(template::Value::List(urls));
    }

    // (list-schemas)
    if code.starts_with("(list-schemas") {
        let graph = conductor.site_graph();
        let mut stems: Vec<String> = graph
            .iter_pages_by_kind(site_index::PageKind::Item)
            .filter_map(|n| n.page_data().map(|pd| pd.schema_stem.as_str().to_string()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        stems.sort();
        let values: Vec<template::Value> = stems
            .into_iter()
            .map(template::Value::Text)
            .collect();
        return Ok(template::Value::List(values));
    }

    Err(format!("unknown expression: {code}"))
}

/// Build url_index and stem_index from conductor's SiteGraph (for eval_repl).
fn repl_build_indexes(
    conductor: &conductor::Conductor,
) -> (expressions::UrlIndex, expressions::StemIndex) {
    let (url_index, stem_index, _) = expressions::build_indexes_from_graph(&conductor.site_graph());
    (url_index, stem_index)
}

/// Extract a string argument from a form like `(get-content "path")` (for eval_repl).
fn repl_extract_string_arg(code: &str) -> Result<String, String> {
    let start = code.find('"').ok_or("expected string argument")?;
    let end = code[start + 1..].find('"').ok_or("unterminated string")?;
    Ok(code[start + 1..start + 1 + end].to_string())
}

/// Extract a keyword argument from a form like `(get-schema :post)` (for eval_repl).
fn repl_extract_keyword_arg(code: &str) -> Result<String, String> {
    let start = code.find(':').ok_or("expected keyword argument")?;
    let rest = &code[start + 1..];
    let end = rest
        .find(|c: char| c == ')' || c.is_whitespace())
        .unwrap_or(rest.len());
    Ok(rest[..end].to_string())
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
            matches!(&result, template::Value::Integer(3)),
            "expected Integer(3), got {result:?}"
        );
    }

    #[test]
    fn subtract_two_numbers() {
        let cond = empty_conductor();
        let result = eval_str("(- 10 3)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Integer(7)));
    }

    #[test]
    fn multiply_two_numbers() {
        let cond = empty_conductor();
        let result = eval_str("(* 4 5)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Integer(20)));
    }

    #[test]
    fn negate_single_number() {
        let cond = empty_conductor();
        let result = eval_str("(- 5)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Integer(-5)));
    }

    #[test]
    fn equality_true() {
        let cond = empty_conductor();
        let result = eval_str("(= 1 1)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn equality_false() {
        let cond = empty_conductor();
        let result = eval_str("(= 1 2)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Bool(false)));
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
    fn integer_literal_evaluates_to_integer() {
        let cond = empty_conductor();
        let result = eval_str("42", &cond).unwrap();
        assert!(matches!(&result, template::Value::Integer(42)));
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
        assert!(matches!(&result, template::Value::Integer(9)));
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
        assert!(matches!(&result, template::Value::Integer(3)));
    }

    #[test]
    fn first_vector() {
        let cond = empty_conductor();
        let result = eval_str("(first [1 2 3])", &cond).unwrap();
        // Integers in a vector evaluate to Value::Integer
        assert!(matches!(&result, template::Value::Integer(1)));
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
            // After reverse, first item should be Integer(3) (was the last)
            assert!(matches!(&items[0], template::Value::Integer(3)));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn take_from_vector() {
        let cond = empty_conductor();
        let result = eval_str("(take 3 [1 2 3 4 5])", &cond).unwrap();
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

    // ── refs-to / refs-from ──────────────────────────────────────────────────

    /// Build a conductor whose site graph contains two posts, one of which has a
    /// PathRef LinkExpression pointing at /author/alice.
    fn linked_conductor() -> conductor::Conductor {
        use std::collections::HashSet;
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .build();
        let cond = conductor::Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap();

        let mut graph = site_index::SiteGraph::new();

        // post/with-link has a link expression → /author/alice
        let mut data_with_link = template::DataGraph::new();
        data_with_link.insert("title", template::Value::Text("Post With Link".into()));
        data_with_link.insert(
            "author",
            template::Value::LinkExpression {
                text: content::LinkText::Empty,
                target: content::LinkTarget::PathRef("/author/alice".to_string()),
            },
        );

        let url_with_link = site_index::UrlPath::new("/post/with-link");
        graph.insert(site_index::SiteNode {
            url_path: url_with_link,
            output_path: PathBuf::from("output/post/with-link/index.html"),
            source_path: PathBuf::from("content/post/with-link.md"),
            deps: HashSet::new(),
            role: site_index::NodeRole::Page(site_index::PageData {
                page_kind: site_index::PageKind::Item,
                schema_stem: site_index::SchemaStem::new("post"),
                template_path: PathBuf::from("templates/post/item.hiccup"),
                content_path: PathBuf::from("content/post/with-link.md"),
                schema_path: PathBuf::from("schemas/post/item.md"),
                data: data_with_link,
            }),
        });

        // post/no-link has no link expression
        let mut data_no_link = template::DataGraph::new();
        data_no_link.insert("title", template::Value::Text("Post Without Link".into()));

        let url_no_link = site_index::UrlPath::new("/post/no-link");
        graph.insert(site_index::SiteNode {
            url_path: url_no_link,
            output_path: PathBuf::from("output/post/no-link/index.html"),
            source_path: PathBuf::from("content/post/no-link.md"),
            deps: HashSet::new(),
            role: site_index::NodeRole::Page(site_index::PageData {
                page_kind: site_index::PageKind::Item,
                schema_stem: site_index::SchemaStem::new("post"),
                template_path: PathBuf::from("templates/post/item.hiccup"),
                content_path: PathBuf::from("content/post/no-link.md"),
                schema_path: PathBuf::from("schemas/post/item.md"),
                data: data_no_link,
            }),
        });

        cond.set_site_graph(graph);
        cond
    }

    #[test]
    fn refs_to_returns_edges_pointing_at_target() {
        let cond = linked_conductor();
        let result = eval_str(r#"(refs-to "/author/alice")"#, &cond).unwrap();
        if let template::Value::List(edges) = result {
            assert_eq!(edges.len(), 1, "expected 1 edge pointing to /author/alice");
            let edge = &edges[0];
            if let template::Value::Record(r) = edge {
                let source = r.resolve(&["source"]).and_then(|v| v.display_text());
                let target = r.resolve(&["target"]).and_then(|v| v.display_text());
                assert_eq!(source.as_deref(), Some("/post/with-link"));
                assert_eq!(target.as_deref(), Some("/author/alice"));
            } else {
                panic!("expected edge to be a Record, got {edge:?}");
            }
        } else {
            panic!("expected List from refs-to");
        }
    }

    #[test]
    fn refs_to_unknown_target_returns_empty_list() {
        let cond = linked_conductor();
        let result = eval_str(r#"(refs-to "/author/nobody")"#, &cond).unwrap();
        assert!(
            matches!(result, template::Value::List(ref v) if v.is_empty()),
            "expected empty list for unknown target"
        );
    }

    #[test]
    fn refs_from_returns_edges_from_source() {
        let cond = linked_conductor();
        let result = eval_str(r#"(refs-from "/post/with-link")"#, &cond).unwrap();
        if let template::Value::List(edges) = result {
            assert_eq!(edges.len(), 1, "expected 1 edge from /post/with-link");
            let edge = &edges[0];
            if let template::Value::Record(r) = edge {
                let source = r.resolve(&["source"]).and_then(|v| v.display_text());
                let target = r.resolve(&["target"]).and_then(|v| v.display_text());
                assert_eq!(source.as_deref(), Some("/post/with-link"));
                assert_eq!(target.as_deref(), Some("/author/alice"));
            } else {
                panic!("expected edge to be a Record, got {edge:?}");
            }
        } else {
            panic!("expected List from refs-from");
        }
    }

    #[test]
    fn refs_from_page_with_no_links_returns_empty_list() {
        let cond = linked_conductor();
        let result = eval_str(r#"(refs-from "/post/no-link")"#, &cond).unwrap();
        assert!(
            matches!(result, template::Value::List(ref v) if v.is_empty()),
            "expected empty list for page with no links"
        );
    }

    #[test]
    fn refs_to_requires_argument() {
        let cond = empty_conductor();
        let result = eval_str("(refs-to)", &cond);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("refs-to requires 1 argument"));
    }

    #[test]
    fn refs_from_requires_argument() {
        let cond = empty_conductor();
        let result = eval_str("(refs-from)", &cond);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("refs-from requires 1 argument"));
    }

    // ── eval_repl tests ──────────────────────────────────────────────────────

    #[test]
    fn eval_repl_empty_returns_absent() {
        let conductor = empty_conductor();
        let result = eval_repl("", &conductor).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn eval_repl_whitespace_returns_absent() {
        let conductor = empty_conductor();
        let result = eval_repl("   ", &conductor).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn eval_repl_bare_keyword_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl(":post", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 post items");
            for item in &items {
                assert!(
                    matches!(item, template::Value::Record(_)),
                    "each item should be a record"
                );
            }
        }
    }

    #[test]
    fn eval_repl_bare_keyword_unknown_stem_returns_empty_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl(":nonexistent", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(ref v) if v.is_empty()));
    }

    #[test]
    fn eval_repl_get_schema_returns_text() {
        let conductor = empty_conductor();
        let result = eval_repl("(get-schema :post)", &conductor).unwrap();
        assert!(matches!(result, template::Value::Text(_)));
        if let template::Value::Text(src) = result {
            assert!(src.contains("Post title"), "schema text should contain 'Post title'");
        }
    }

    #[test]
    fn eval_repl_get_schema_unknown_returns_error() {
        let conductor = empty_conductor();
        let result = eval_repl("(get-schema :nonexistent)", &conductor);
        assert!(result.is_err(), "expected error for unknown schema");
    }

    #[test]
    fn eval_repl_list_content_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl("(list-content)", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 content items");
        }
    }

    #[test]
    fn eval_repl_list_schemas_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl("(list-schemas)", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 1, "expected 1 unique stem (post)");
            assert!(
                matches!(&items[0], template::Value::Text(s) if s == "post"),
                "expected stem 'post'"
            );
        }
    }

    #[test]
    fn eval_repl_unknown_expression_returns_error() {
        let conductor = empty_conductor();
        let result = eval_repl("(frobnicate :foo)", &conductor);
        assert!(result.is_err(), "expected error for unknown expression");
        let msg = result.unwrap_err();
        assert!(msg.contains("unknown expression"), "error should mention 'unknown expression'");
    }

    #[test]
    fn eval_repl_thread_expr_returns_list() {
        let (_dir, conductor) = two_post_conductor();
        let result = eval_repl("(->> :post)", &conductor).unwrap();
        assert!(matches!(result, template::Value::List(_)));
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 2, "expected 2 post items from thread expr");
        }
    }

    // ── Phase 2: Env, closures, and special forms (ADR-036) ──────────────────

    // ── let ─────────────────────────────────────────────────────────────────

    #[test]
    fn let_single_binding() {
        let cond = empty_conductor();
        let result = eval_str("(let [x 1] x)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(1)));
    }

    #[test]
    fn let_multiple_bindings() {
        let cond = empty_conductor();
        let result = eval_str("(let [x 1 y 2] (+ x y))", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn let_sequential_bindings() {
        // y can see x because bindings are sequential
        let cond = empty_conductor();
        let result = eval_str("(let [x 5 y (+ x 1)] y)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(6)));
    }

    #[test]
    fn let_multi_body_returns_last() {
        let cond = empty_conductor();
        let result = eval_str("(let [x 1] x x 42)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    // ── if ──────────────────────────────────────────────────────────────────

    #[test]
    fn if_true_branch() {
        let cond = empty_conductor();
        let result = eval_str("(if true 1 2)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(1)));
    }

    #[test]
    fn if_false_branch() {
        let cond = empty_conductor();
        let result = eval_str("(if false 1 2)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn if_nil_is_falsy() {
        let cond = empty_conductor();
        let result = eval_str("(if nil 1 2)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn if_zero_is_truthy() {
        // 0 is truthy in Clojure (only false and nil are falsy)
        let cond = empty_conductor();
        let result = eval_str("(if 0 1 2)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(1)));
    }

    #[test]
    fn if_no_else_returns_absent() {
        let cond = empty_conductor();
        let result = eval_str("(if false 1)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── do ──────────────────────────────────────────────────────────────────

    #[test]
    fn do_returns_last_value() {
        let cond = empty_conductor();
        let result = eval_str("(do 1 2 3)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn do_empty_returns_absent() {
        let cond = empty_conductor();
        let result = eval_str("(do)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── fn and closures ──────────────────────────────────────────────────────

    #[test]
    fn fn_immediate_application() {
        // ((fn [x] (+ x 1)) 10) → 11
        let cond = empty_conductor();
        let result = eval_str("((fn [x] (+ x 1)) 10)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(11)));
    }

    #[test]
    fn fn_captures_lexical_scope() {
        // (let [n 5] ((fn [x] (+ x n)) 3)) → 8
        let cond = empty_conductor();
        let result = eval_str("(let [n 5] ((fn [x] (+ x n)) 3))", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(8)));
    }

    #[test]
    fn fn_stored_in_let() {
        // (let [f (fn [x] (* x 2))] (f 5)) → 10
        let cond = empty_conductor();
        let result = eval_str("(let [f (fn [x] (* x 2))] (f 5))", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(10)));
    }

    #[test]
    fn fn_variadic_rest_param() {
        // ((fn [x & rest] rest) 1 2 3) → [2 3]
        let cond = empty_conductor();
        let result = eval_str("((fn [x & rest] rest) 1 2 3)", &cond).unwrap();
        assert!(
            matches!(&result, template::Value::List(items) if items.len() == 2),
            "expected List of 2, got {result:?}"
        );
    }

    #[test]
    fn fn_produces_fn_value() {
        let cond = empty_conductor();
        let result = eval_str("(fn [x] x)", &cond).unwrap();
        assert!(matches!(result, template::Value::Fn(_)));
    }

    // ── def ─────────────────────────────────────────────────────────────────
    // Note: def bindings are per-eval call (root env is not persisted across calls).
    // Within a single expression, def+usage works if we use let or fn body.

    #[test]
    fn def_in_do_block() {
        // (do (def x 42) x) — def persists within the same eval call's root
        let cond = empty_conductor();
        let result = eval_str("(do (def x 42) x)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    // ── defn macro ───────────────────────────────────────────────────────────

    #[test]
    fn defn_defines_and_calls_function() {
        // (do (defn double [x] (* x 2)) (double 5)) → 10
        let cond = empty_conductor();
        let result = eval_str("(do (defn double [x] (* x 2)) (double 5))", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(10)));
    }

    // ── quote ────────────────────────────────────────────────────────────────

    #[test]
    fn quote_returns_form_as_data() {
        let cond = empty_conductor();
        // (quote (+ 1 2)) → a List value, not the result of addition
        let result = eval_str("(quote (+ 1 2))", &cond).unwrap();
        // The quoted form should be a List with 3 elements
        assert!(
            matches!(&result, template::Value::List(items) if items.len() == 3),
            "expected List of 3, got {result:?}"
        );
    }

    #[test]
    fn quote_keyword() {
        let cond = empty_conductor();
        let result = eval_str("(quote :foo)", &cond).unwrap();
        assert!(
            matches!(&result, template::Value::Keyword { name, .. } if name == "foo"),
            "got {result:?}"
        );
    }

    // ── Value::Integer, Value::Bool, Value::Keyword ──────────────────────────

    #[test]
    fn bool_literal_true() {
        let cond = empty_conductor();
        let result = eval_str("true", &cond).unwrap();
        assert!(matches!(result, template::Value::Bool(true)));
    }

    #[test]
    fn bool_literal_false() {
        let cond = empty_conductor();
        let result = eval_str("false", &cond).unwrap();
        assert!(matches!(result, template::Value::Bool(false)));
    }

    // ── macro expansions (when, cond, and, or) ───────────────────────────────

    #[test]
    fn when_true_evaluates_body() {
        let cond = empty_conductor();
        let result = eval_str("(when true 42)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn when_false_returns_nil() {
        let cond = empty_conductor();
        let result = eval_str("(when false 42)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn when_not_true_returns_nil() {
        let cond = empty_conductor();
        let result = eval_str("(when-not true 42)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn when_not_false_evaluates_body() {
        let cond = empty_conductor();
        let result = eval_str("(when-not false 42)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn cond_first_matching_clause() {
        let cond = empty_conductor();
        let result = eval_str("(cond false 1 true 2 true 3)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn cond_no_match_returns_nil() {
        let cond = empty_conductor();
        let result = eval_str("(cond false 1 false 2)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn and_all_true() {
        let cond = empty_conductor();
        let result = eval_str("(and 1 2 3)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn and_short_circuits_on_false() {
        let cond = empty_conductor();
        let result = eval_str("(and 1 false 3)", &cond).unwrap();
        assert!(matches!(result, template::Value::Bool(false)));
    }

    #[test]
    fn and_empty_returns_true() {
        let cond = empty_conductor();
        let result = eval_str("(and)", &cond).unwrap();
        assert!(matches!(result, template::Value::Bool(true)));
    }

    #[test]
    fn or_first_truthy() {
        let cond = empty_conductor();
        let result = eval_str("(or false nil 42)", &cond).unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn or_all_false_returns_nil() {
        let cond = empty_conductor();
        let result = eval_str("(or false nil)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn or_empty_returns_nil() {
        let cond = empty_conductor();
        let result = eval_str("(or)", &cond).unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── map with fn ──────────────────────────────────────────────────────────

    #[test]
    fn map_with_fn_value() {
        // (map (fn [x] (+ x 1)) [1 2 3]) → [2 3 4]
        let cond = empty_conductor();
        let result = eval_str("(map (fn [x] (+ x 1)) [1 2 3])", &cond).unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], template::Value::Integer(2)));
            assert!(matches!(items[1], template::Value::Integer(3)));
            assert!(matches!(items[2], template::Value::Integer(4)));
        } else {
            panic!("expected list");
        }
    }

}
