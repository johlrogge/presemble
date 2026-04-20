mod env;
mod closure;
pub mod primitives;
pub mod doc_registry;
pub mod ned_primitives;

pub use env::{Env, RootEnv};
pub use closure::{Closure, FnArity, PrimitiveFn};
pub use doc_registry::{DocEntry, DocRegistry, DocSource};

use std::sync::Arc;
use forms::Form;

/// The core.clj prelude — embedded at compile time.
const PRELUDE: &str = include_str!("core.clj");

/// The ned.clj prelude — embedded at compile time.
const NED_PRELUDE: &str = include_str!("ned.clj");

/// Load the core.clj prelude into the root environment.
/// Called once after `register_builtins`, before any user evaluation.
fn load_prelude(root: &RootEnv) -> Result<(), String> {
    let forms = reader::read_all(PRELUDE)
        .map_err(|e| format!("prelude read error: {e}"))?;
    let env = root.snapshot();
    for form in forms {
        let expanded = macros::macroexpand(form);
        eval_in_env(&expanded, &env, root)?;
    }
    Ok(())
}

/// Load the ned.clj prelude into the root environment.
/// Called after `register_ned_builtins`, before any user evaluation.
pub fn load_ned_prelude(root: &RootEnv) -> Result<(), String> {
    let forms = reader::read_all(NED_PRELUDE)
        .map_err(|e| format!("ned prelude read error: {e}"))?;
    let env = root.snapshot();
    for form in forms {
        let expanded = macros::macroexpand(form);
        eval_in_env(&expanded, &env, root)?;
    }
    Ok(())
}

/// Evaluate a form in a fresh root environment (no conductor).
pub fn eval(form: &Form) -> Result<template::Value, String> {
    let expanded = macros::macroexpand(form.clone());
    let root = RootEnv::new();
    primitives::register_builtins(&root);
    register_higher_order_builtins(&root);
    register_macro_docs(&root.doc_registry);
    load_prelude(&root).map_err(|e| format!("prelude load failed: {e}"))?;
    let env = root.snapshot();
    eval_in_env(&expanded, &env, &root)
}

/// Evaluate a string expression (read + macroexpand + eval).
/// No conductor needed — conductor-specific functions are not available.
/// Use `eval_str_with_root` with a root that has `register_conductor_builtins`
/// called on it if you need conductor functions.
pub fn eval_str(code: &str) -> Result<template::Value, String> {
    let form = reader::read(code).map_err(|e| format!("read error: {e}"))?;
    eval(&form)
}

/// Evaluate a string expression against a persistent root environment.
/// Used by the REPL so that `def` forms accumulate across evaluations.
/// The caller is responsible for initializing `root` with `init_root`
/// (and optionally `register_conductor_builtins`) before the first call.
pub fn eval_str_with_root(
    code: &str,
    root: &RootEnv,
) -> Result<template::Value, String> {
    let form = reader::read(code).map_err(|e| format!("read error: {e}"))?;
    let expanded = macros::macroexpand(form);
    let env = root.snapshot();
    eval_in_env(&expanded, &env, root)
}

/// Initialise a `RootEnv` with builtins, macro docs, and the prelude.
/// Returns `Err` if the prelude fails to load.
/// After calling this, optionally call `register_conductor_builtins` to add
/// conductor-specific functions.
pub fn init_root(root: &RootEnv) -> Result<(), String> {
    primitives::register_builtins(root);
    register_higher_order_builtins(root);
    register_macro_docs(&root.doc_registry);
    load_prelude(root)
}

/// Internal evaluation entry point — used by closures calling back into the evaluator.
/// Takes an explicit lexical environment and root env.
pub(crate) fn eval_in_env(
    form: &Form,
    env: &Arc<Env>,
    root: &RootEnv,
) -> Result<template::Value, String> {
    match form {
        // Literals evaluate to themselves (ADR-036 Phase 1: proper types)
        Form::Str(s) => Ok(template::Value::Text(s.clone())),
        Form::Integer(n) => Ok(template::Value::Integer(*n)),
        Form::Bool(b) => Ok(template::Value::Bool(*b)),
        Form::Nil => Ok(template::Value::Absent),

        // Keywords are first-class values (ADR-036 Phase 6)
        // Use (query :stem) for explicit stem queries.
        Form::Keyword { namespace, name } => Ok(template::Value::Keyword {
            namespace: namespace.clone(),
            name: name.clone(),
        }),

        // Vectors evaluate each element
        Form::Vector(items) => {
            let values: Vec<template::Value> = items
                .iter()
                .map(|item| eval_in_env(item, env, root))
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
                let val = eval_in_env(v, env, root)?;
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
                let map_val = eval_in_env(&items[1], env, root)?;
                let default = if items.len() > 2 {
                    eval_in_env(&items[2], env, root)?
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
                        let test = eval_in_env(&items[1], env, root)?;
                        let is_truthy = !matches!(&test, template::Value::Bool(false) | template::Value::Absent);
                        if is_truthy {
                            return eval_in_env(&items[2], env, root);
                        } else if items.len() > 3 {
                            return eval_in_env(&items[3], env, root);
                        } else {
                            return Ok(template::Value::Absent);
                        }
                    }

                    // ── (do form...) ─────────────────────────────────────────
                    "do" => {
                        let mut result = template::Value::Absent;
                        for form in &items[1..] {
                            result = eval_in_env(form, env, root)?;
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
                        let val = eval_in_env(&items[2], env, root)?;
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
                            let val = eval_in_env(&chunk[1], &local_arc, root)?;
                            local = local.set(bind_name, val);
                        }

                        let local = Arc::new(local);
                        let mut result = template::Value::Absent;
                        for form in &items[2..] {
                            result = eval_in_env(form, &local, root)?;
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

                    // ── (def-doc! name "docstring" "[args]" ...) ─────────────
                    "def-doc!" => {
                        if items.len() < 3 {
                            return Err("def-doc! requires at least name and docstring".into());
                        }
                        let name = items[1]
                            .as_symbol()
                            .ok_or_else(|| "def-doc!: first argument must be a symbol".to_string())?;
                        let doc = match &items[2] {
                            Form::Str(s) => s.clone(),
                            _ => return Err("def-doc!: second argument must be a docstring".into()),
                        };
                        let arglists: Vec<String> = items[3..]
                            .iter()
                            .filter_map(|f| f.as_str().map(String::from))
                            .collect();
                        root.doc_registry.register(crate::doc_registry::DocEntry {
                            name: name.to_string(),
                            doc,
                            arglists,
                            source: crate::doc_registry::DocSource::Prelude,
                        });
                        return Ok(template::Value::Absent);
                    }

                    // ── (doc sym) — special form: arg not evaluated ──────────
                    // `doc` does not evaluate its argument so that (doc +) and
                    // (doc ->) look up the name, not the function value.
                    "doc" => {
                        if items.len() == 1 {
                            // (doc) with no args — fall through to the PrimitiveFn with empty vec
                            if let Some(template::Value::Fn(callable)) = root.get("doc") {
                                return callable.call(vec![]);
                            }
                            return Err("doc: no doc function registered".into());
                        }
                        // Extract name from the unevaluated argument form.
                        let sym_name = match &items[1] {
                            Form::Symbol(s) => s.clone(),
                            Form::Str(s) => s.clone(),
                            Form::Keyword { name, .. } => name.clone(),
                            other => {
                                let val = eval_in_env(other, env, root)?;
                                if let Some(template::Value::Fn(callable)) = root.get("doc") {
                                    return callable.call(vec![val]);
                                }
                                return Err("doc: no doc function registered".into());
                            }
                        };
                        if let Some(template::Value::Fn(callable)) = root.get("doc") {
                            return callable.call(vec![template::Value::Text(sym_name)]);
                        }
                        return Err("doc: no doc function registered".into());
                    }

                    _ => {} // fall through to function call dispatch
                }
            }

            // ── Function call dispatch ───────────────────────────────────────

            // Evaluate the function position (may be a symbol, lambda, etc.)
            let func_val = eval_in_env(func_form, env, root).map_err(|e| {
                if let Some(name) = func_form.as_symbol()
                    && e.contains("unbound symbol")
                {
                    return format!("unknown function: {name}");
                }
                e
            })?;

            // Evaluate arguments eagerly.
            let args: Vec<template::Value> = items[1..]
                .iter()
                .map(|a| eval_in_env(a, env, root))
                .collect::<Result<_, _>>()?;

            match func_val {
                template::Value::Fn(ref callable) => callable.call(args),
                other => Err(format!("not a function: {other:?} (from {func_form})")),
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

// ── Higher-order builtin functions ─────────────────────────────────────────

/// Apply a `Value` (keyword or callable) to a single item value.
/// This is the core helper for map, filter, sort-by, every?, some.
fn apply_value_to_value(func: &template::Value, val: &template::Value) -> Result<template::Value, String> {
    match func {
        template::Value::Keyword { namespace: None, name } => {
            match val {
                template::Value::Record(r) => Ok(r.resolve(&[name.as_str()]).cloned().unwrap_or(template::Value::Absent)),
                _ => Ok(template::Value::Absent),
            }
        }
        template::Value::Fn(callable) => callable.call(vec![val.clone()]),
        other => Err(format!("not a function: {other:?}")),
    }
}

/// Apply a `Value` (keyword or callable) to two args (for reduce).
fn apply_value_to_two(func: &template::Value, a: template::Value, b: template::Value) -> Result<template::Value, String> {
    match func {
        template::Value::Fn(callable) => callable.call(vec![a, b]),
        other => Err(format!("reduce: not a function: {other:?}")),
    }
}

/// Apply a `Value` (keyword or callable) to a list of args.
fn call_value(func: &template::Value, args: Vec<template::Value>) -> Result<template::Value, String> {
    match func {
        template::Value::Keyword { name, .. } => {
            match args.first() {
                Some(template::Value::Record(r)) => Ok(r.resolve(&[name.as_str()]).cloned().unwrap_or(template::Value::Absent)),
                _ => Ok(template::Value::Absent),
            }
        }
        template::Value::Fn(callable) => callable.call(args),
        other => Err(format!("not a function: {other:?}")),
    }
}

/// Helper: register a named PrimitiveFn with documentation in the root env.
fn prim_reg(
    root: &RootEnv,
    name: &'static str,
    arglist: &'static str,
    doc: &'static str,
    f: impl Fn(Vec<template::Value>) -> Result<template::Value, String> + Send + Sync + 'static,
) {
    use crate::closure::PrimitiveFn;
    use crate::doc_registry::{DocEntry, DocSource};
    let prim = PrimitiveFn::new(name, f);
    root.def(name.to_string(), prim.into_value());
    root.doc_registry.register(DocEntry {
        name: name.to_string(),
        doc: doc.to_string(),
        arglists: vec![arglist.to_string()],
        source: DocSource::Primitive,
    });
}

/// Register higher-order functions (map, filter, reduce, apply, every?, some, sort-by,
/// println, doc) as PrimitiveFn entries in the root environment.
/// These do not need conductor access.
pub fn register_higher_order_builtins(root: &RootEnv) {
    prim_reg(root, "map", "(map f coll)", "Apply f to each item in coll.", |args: Vec<template::Value>| {
        if args.len() < 2 {
            return Err("map requires 2 arguments: function and collection".into());
        }
        let func = &args[0];
        let items = match &args[args.len() - 1] {
            template::Value::List(items) => items.clone(),
            _ => return Err("map expects a list as the last argument".into()),
        };
        let results: Vec<template::Value> = items
            .into_iter()
            .map(|item| apply_value_to_value(func, &item))
            .collect::<Result<_, _>>()?;
        Ok(template::Value::List(results))
    });

    prim_reg(root, "filter", "(filter pred coll) or (filter :field val coll)", "Filter items by predicate or field value.", |args: Vec<template::Value>| {
        if args.len() < 2 {
            return Err("filter requires at least 2 arguments".into());
        }
        // 2-arg form: (filter pred coll)
        if args.len() == 2 {
            let pred = &args[0];
            let items = match &args[1] {
                template::Value::List(items) => items.clone(),
                _ => return Err("filter expects a list".into()),
            };
            let filtered: Vec<template::Value> = items
                .into_iter()
                .filter_map(|item| {
                    let result = apply_value_to_value(pred, &item).ok()?;
                    match result {
                        template::Value::Bool(false) | template::Value::Absent => None,
                        _ => Some(item),
                    }
                })
                .collect();
            return Ok(template::Value::List(filtered));
        }
        // 3-arg form: (filter :field value coll)
        let field = match &args[0] {
            template::Value::Keyword { name, .. } => name.clone(),
            _ => return Err("filter: 3-arg form requires a keyword as first argument".into()),
        };
        let target_str = primitives::value_to_string(&args[1]);
        let items = match &args[2] {
            template::Value::List(items) => items.clone(),
            _ => return Err("filter expects a list".into()),
        };
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
    });

    prim_reg(root, "reduce", "(reduce f coll) or (reduce f init coll)", "Fold over a collection.", |args: Vec<template::Value>| {
        if args.len() < 2 {
            return Err("reduce requires at least 2 arguments: fn and collection".into());
        }
        let func = &args[0];
        let (init, items) = if args.len() == 2 {
            let items = match &args[1] {
                template::Value::List(items) => items.clone(),
                _ => return Err("reduce expects a list".into()),
            };
            (None, items)
        } else {
            let items = match &args[2] {
                template::Value::List(items) => items.clone(),
                _ => return Err("reduce expects a list".into()),
            };
            (Some(args[1].clone()), items)
        };
        let mut iter = items.into_iter();
        let mut acc = match init {
            Some(v) => v,
            None => iter.next().unwrap_or(template::Value::Absent),
        };
        for item in iter {
            acc = apply_value_to_two(func, acc, item)?;
        }
        Ok(acc)
    });

    prim_reg(root, "apply", "(apply f arg1 ... args-list)", "Apply function to list of arguments.", |args: Vec<template::Value>| {
        if args.len() < 2 {
            return Err("apply requires at least 2 arguments: fn and args-list".into());
        }
        let func = &args[0];
        let last = args.last().unwrap();
        let coll_args = match last {
            template::Value::List(items) => items.clone(),
            _ => return Err("apply: last argument must be a list".into()),
        };
        let mut all_args: Vec<template::Value> = args[1..args.len() - 1].to_vec();
        all_args.extend(coll_args);
        call_value(func, all_args)
    });

    prim_reg(root, "every?", "(every? pred coll)", "True if pred returns truthy for all items.", |args: Vec<template::Value>| {
        if args.len() != 2 {
            return Err("every? requires 2 arguments: pred and coll".into());
        }
        let func = &args[0];
        match &args[1] {
            template::Value::List(items) => {
                for item in items {
                    let result = apply_value_to_value(func, item)?;
                    if matches!(result, template::Value::Bool(false) | template::Value::Absent) {
                        return Ok(template::Value::Bool(false));
                    }
                }
                Ok(template::Value::Bool(true))
            }
            _ => Err("every? expects a list as second argument".into()),
        }
    });

    prim_reg(root, "some", "(some pred coll)", "First truthy result of pred applied to items, or nil.", |args: Vec<template::Value>| {
        if args.len() != 2 {
            return Err("some requires 2 arguments: pred and coll".into());
        }
        let func = &args[0];
        match &args[1] {
            template::Value::List(items) => {
                for item in items {
                    let result = apply_value_to_value(func, item)?;
                    if !matches!(result, template::Value::Bool(false) | template::Value::Absent) {
                        return Ok(result);
                    }
                }
                Ok(template::Value::Absent)
            }
            _ => Err("some requires a list as second argument".into()),
        }
    });

    prim_reg(root, "sort-by", "(sort-by :field coll) or (sort-by :field :desc coll)", "Sort list by keyword field.", |args: Vec<template::Value>| {
        if args.len() < 2 {
            return Err("sort-by requires at least 2 arguments: field and collection".into());
        }
        let field = match &args[0] {
            template::Value::Keyword { name, .. } => name.clone(),
            _ => return Err("sort-by field must be a keyword".into()),
        };
        let descending = args.len() > 2
            && matches!(&args[1], template::Value::Keyword { name, .. } if name == "desc");
        let mut items = match &args[args.len() - 1] {
            template::Value::List(items) => items.clone(),
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
            if descending { b_val.cmp(&a_val) } else { a_val.cmp(&b_val) }
        });
        Ok(template::Value::List(items))
    });

    prim_reg(root, "println", "(println val ...)", "Print values to stderr, separated by spaces.", |args: Vec<template::Value>| {
        let parts: Vec<String> = args.iter().map(primitives::value_to_string).collect();
        eprintln!("{}", parts.join(" "));
        Ok(template::Value::Absent)
    });

    // Register `doc` capturing the doc_registry (Arc-backed, so clone shares state).
    let doc_reg = root.doc_registry.clone();
    {
        let doc_reg_clone = doc_reg.clone();
        let prim = PrimitiveFn::new("doc", move |args: Vec<template::Value>| {
            if args.is_empty() {
                // (doc) — list all documented symbols
                let entries = doc_reg_clone.all_entries();
                let mut help = String::from("Available functions:\n\n");
                let primitives_list: Vec<_> = entries
                    .iter()
                    .filter(|e| matches!(e.source, crate::doc_registry::DocSource::Primitive))
                    .collect();
                let prelude_list: Vec<_> = entries
                    .iter()
                    .filter(|e| matches!(e.source, crate::doc_registry::DocSource::Prelude))
                    .collect();
                let user_list: Vec<_> = entries
                    .iter()
                    .filter(|e| matches!(e.source, crate::doc_registry::DocSource::User))
                    .collect();
                if !primitives_list.is_empty() {
                    help.push_str("── Primitives ──\n");
                    for e in &primitives_list {
                        help.push_str(&format!("  {:<20} {}\n", e.name, first_line(&e.doc)));
                    }
                    help.push('\n');
                }
                if !prelude_list.is_empty() {
                    help.push_str("── Core library ──\n");
                    for e in &prelude_list {
                        help.push_str(&format!("  {:<20} {}\n", e.name, first_line(&e.doc)));
                    }
                    help.push('\n');
                }
                if !user_list.is_empty() {
                    help.push_str("── User-defined ──\n");
                    for e in &user_list {
                        help.push_str(&format!("  {:<20} {}\n", e.name, first_line(&e.doc)));
                    }
                }
                return Ok(template::Value::Text(help));
            }
            let name = match &args[0] {
                template::Value::Text(s) => s.as_str().to_string(),
                template::Value::Keyword { name, .. } => name.clone(),
                // When a symbol like `+` is evaluated it becomes a Value::Fn;
                // extract the function name to look up docs.
                template::Value::Fn(callable) => {
                    match callable.name() {
                        Some(n) => n.to_string(),
                        None => return Err("doc: anonymous function has no documentation".into()),
                    }
                }
                other => return Err(format!("doc expects a symbol or string, got: {other:?}")),
            };
            match doc_reg_clone.lookup(&name) {
                Some(entry) => {
                    let mut result = format!("{}\n", entry.name);
                    for arglist in &entry.arglists {
                        result.push_str(&format!("  {}\n", arglist));
                    }
                    result.push_str(&format!("  {}\n", entry.doc));
                    result.push_str(&format!("  Source: {:?}\n", entry.source));
                    Ok(template::Value::Text(result))
                }
                None => Err(format!("no documentation for: {name}")),
            }
        });
        root.def("doc".to_string(), prim.into_value());
        root.doc_registry.register(DocEntry {
            name: "doc".to_string(),
            doc: "Show documentation for a function. With no args, lists all functions.".to_string(),
            arglists: vec!["(doc name)".to_string(), "(doc)".to_string()],
            source: DocSource::Primitive,
        });
    }
}

// ── Documentation ───────────────────────────────────────────────────────────

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// Register documentation for macros and special forms into the doc registry.
pub fn register_macro_docs(registry: &crate::doc_registry::DocRegistry) {
    use crate::doc_registry::{DocEntry, DocSource};

    let macros: &[(&str, &str, &str)] = &[
        ("->",      "(-> x (f a) (g b) ...)",      "Thread-first: insert x as first arg of each form left-to-right."),
        ("->>",     "(->> x (f a) (g b) ...)",     "Thread-last: insert x as last arg of each form left-to-right."),
        ("as->",    "(as-> expr name form ...)",    "Thread expr as name through each form."),
        ("some->",  "(some-> x (f a) ...)",         "Thread-first, short-circuiting on nil."),
        ("some->>", "(some->> x (f a) ...)",        "Thread-last, short-circuiting on nil."),
        ("cond->",  "(cond-> x test (f a) ...)",    "Thread-first conditionally; does not short-circuit."),
        ("cond->>", "(cond->> x test (f a) ...)",   "Thread-last conditionally; does not short-circuit."),
        ("when",    "(when test body ...)",          "Evaluate body forms when test is truthy, else nil."),
        ("when-not","(when-not test body ...)",      "Evaluate body forms when test is falsy, else nil."),
        ("if-not",  "(if-not test then else?)",      "If test is falsy, evaluate then; otherwise else."),
        ("if-let",  "(if-let [x expr] then else?)", "Bind expr to x; if truthy evaluate then, else else."),
        ("when-let","(when-let [x expr] body ...)", "Bind expr to x; if truthy evaluate body forms."),
        ("cond",    "(cond test expr ... :else expr)", "Evaluate first expr whose test is truthy."),
        ("and",     "(and x y ...)",                 "Short-circuit logical and; returns last truthy value or false."),
        ("or",      "(or x y ...)",                  "Short-circuit logical or; returns first truthy value or nil."),
        ("defn",    "(defn name \"doc?\" [args] body ...)", "Define a named function, optionally with a docstring."),
        // Special forms
        ("if",      "(if test then else?)",          "If test is truthy evaluate then, otherwise else or nil."),
        ("let",     "(let [x v ...] body ...)",      "Bind names to values in local scope."),
        ("do",      "(do form ...)",                 "Evaluate forms in sequence; return last value."),
        ("def",     "(def name value)",              "Define a global binding."),
        ("fn",      "(fn [args] body ...)",          "Create an anonymous function."),
        ("quote",   "(quote form)",                  "Return form unevaluated."),
        ("query",   "(query :stem)",                 "Query all content items for the given schema stem."),
    ];

    for (name, sig, doc) in macros {
        registry.register(DocEntry {
            name: name.to_string(),
            doc: doc.to_string(),
            arglists: vec![sig.to_string()],
            source: DocSource::Primitive,
        });
    }
}

// Note: `register_conductor_builtins` and `eval_repl` have been moved to
// `editor_server` to break the evaluator ↔ conductor circular dependency.
// See `editor_server::register_conductor_builtins`.

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    const POST_SCHEMA_SRC: &str =
        "# Post title {#title}\noccurs\n: exactly once\ncontent\n: capitalized\n\n----\nBody.\n";

    /// Build a conductor with no content using a pre-loaded schema.
    fn empty_conductor() -> Arc<conductor::Conductor> {
        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .build();
        Arc::new(conductor::Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap())
    }

    /// Test-local helper: register conductor-specific builtins.
    /// Mirrors `editor_server::register_conductor_builtins` — kept here to avoid
    /// a compile-time dependency on `editor_server` (a base crate).
    fn register_conductor_builtins(root: &RootEnv, conductor: &Arc<conductor::Conductor>) {
        fn prim_reg(
            root: &RootEnv,
            name: &'static str,
            f: impl Fn(Vec<template::Value>) -> Result<template::Value, String> + Send + Sync + 'static,
        ) {
            let prim = PrimitiveFn::new(name, f);
            root.def(name.to_string(), prim.into_value());
        }

        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "query", move |args| {
                if args.is_empty() { return Err("query requires 1 argument".into()); }
                let stem = match &args[0] {
                    template::Value::Keyword { name, .. } => name.clone(),
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("query expects a keyword or string".into()),
                };
                let items = cond.query_items_for_stem(&stem);
                Ok(template::Value::List(items.into_iter().map(|(url, mut g)| {
                    g.insert("url", template::Value::Text(url));
                    template::Value::Record(g)
                }).collect()))
            });
        }
        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "get-content", move |args| {
                if args.is_empty() { return Err("get-content requires 1 argument".into()); }
                let path_str = match &args[0] {
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("get-content path must be a string".into()),
                };
                let abs_path = cond.site_dir().join(&path_str);
                match cond.document_text(&abs_path) {
                    Some(text) => Ok(template::Value::Text(text)),
                    None => Err(format!("file not found: {path_str}")),
                }
            });
        }
        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "get-schema", move |args| {
                if args.is_empty() { return Err("get-schema requires 1 argument".into()); }
                let stem = match &args[0] {
                    template::Value::Keyword { name, .. } => name.clone(),
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("get-schema argument must be a keyword".into()),
                };
                match cond.schema_source(&stem) {
                    Some(src) => Ok(template::Value::Text(src)),
                    None => Err(format!("no schema for: {stem}")),
                }
            });
        }
        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "list-content", move |_args| {
                Ok(template::Value::List(cond.list_content_urls().into_iter().map(template::Value::Text).collect()))
            });
        }
        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "list-schemas", move |_args| {
                let mut stems = cond.list_schemas();
                stems.sort();
                Ok(template::Value::List(stems.into_iter().map(template::Value::Text).collect()))
            });
        }
        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "refs-to", move |args| {
                if args.is_empty() { return Err("refs-to requires 1 argument: a URL path string".into()); }
                let url_str = match &args[0] {
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("refs-to: argument must be a string URL path".into()),
                };
                let edges = cond.query_edges_to(&url_str);
                Ok(template::Value::List(edges.iter().map(|e| {
                    let mut r = template::DataGraph::new();
                    r.insert("source", template::Value::Text(e.source.as_str().to_string()));
                    r.insert("target", template::Value::Text(e.target.as_str().to_string()));
                    template::Value::Record(r)
                }).collect()))
            });
        }
        {
            let cond = Arc::clone(conductor);
            prim_reg(root, "refs-from", move |args| {
                if args.is_empty() { return Err("refs-from requires 1 argument: a URL path string".into()); }
                let url_str = match &args[0] {
                    template::Value::Text(s) => s.clone(),
                    _ => return Err("refs-from: argument must be a string URL path".into()),
                };
                let edges = cond.query_edges_from(&url_str);
                Ok(template::Value::List(edges.iter().map(|e| {
                    let mut r = template::DataGraph::new();
                    r.insert("source", template::Value::Text(e.source.as_str().to_string()));
                    r.insert("target", template::Value::Text(e.target.as_str().to_string()));
                    template::Value::Record(r)
                }).collect()))
            });
        }
    }

    /// Test-local re-implementation of the legacy string REPL evaluator.
    /// Mirrors the old `eval_repl` that was moved to `editor_server`.
    fn eval_repl(code: &str, conductor: &conductor::Conductor) -> Result<template::Value, String> {
        let code = code.trim();
        if code.is_empty() {
            return Ok(template::Value::Absent);
        }
        if code.starts_with(':') && !code.contains(' ') {
            let stem = &code[1..];
            let items = conductor.query_items_for_stem(stem);
            return Ok(template::Value::List(items.into_iter().map(|(url, mut g)| {
                g.insert("url", template::Value::Text(url));
                template::Value::Record(g)
            }).collect()));
        }
        if code.starts_with("(->>") || code.starts_with("(->") {
            let target = content::parse_link_target(code)
                .map_err(|e| format!("parse error: {e}"))?;
            let text = content::LinkText::Empty;
            let (url_index, stem_index, _) = conductor.build_expression_indexes_from_store_pub();
            let edge_index = expressions::EdgeIndex::new();
            let current_url = site_index::UrlPath::new("/");
            return Ok(expressions::evaluate_link_expression(
                &text, &target, &url_index, &stem_index, &current_url, &edge_index,
            ));
        }
        if code.starts_with("(get-content") {
            let start = code.find('"').ok_or("expected string argument")?;
            let end = code[start + 1..].find('"').ok_or("unterminated string")?;
            let path = &code[start + 1..start + 1 + end];
            let abs_path = conductor.site_dir().join(path);
            return match conductor.document_text(&abs_path) {
                Some(text) => Ok(template::Value::Text(text)),
                None => Err(format!("file not found: {path}")),
            };
        }
        if code.starts_with("(get-schema") {
            let start = code.find(':').ok_or("expected keyword argument")?;
            let rest = &code[start + 1..];
            let end = rest.find(|c: char| c == ')' || c.is_whitespace()).unwrap_or(rest.len());
            let stem = &rest[..end];
            return match conductor.schema_source(stem) {
                Some(src) => Ok(template::Value::Text(src)),
                None => Err(format!("no schema for: {stem}")),
            };
        }
        if code.starts_with("(list-content") {
            return Ok(template::Value::List(conductor.list_content_urls().into_iter().map(template::Value::Text).collect()));
        }
        if code.starts_with("(list-schemas") {
            let mut stems = conductor.list_schemas();
            stems.sort();
            return Ok(template::Value::List(stems.into_iter().map(template::Value::Text).collect()));
        }
        Err(format!("unknown expression: {code}"))
    }

    /// Evaluate a string expression with a conductor's builtins registered.
    fn eval_with_conductor(code: &str, cond: &Arc<conductor::Conductor>) -> Result<template::Value, String> {
        let root = RootEnv::new();
        init_root(&root)?;
        register_conductor_builtins(&root, cond);
        eval_str_with_root(code, &root)
    }

    /// Build a conductor backed by a temp dir with two post content files.
    fn two_post_conductor() -> (tempfile::TempDir, Arc<conductor::Conductor>) {
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
            Arc::new(conductor::Conductor::with_repo(dir.path().to_path_buf(), repo).unwrap());
        (dir, conductor)
    }

    // ── arithmetic ──────────────────────────────────────────────────────────

    #[test]
    fn add_two_numbers() {
        let result = eval_str("(+ 1 2)").unwrap();
        assert!(
            matches!(&result, template::Value::Integer(3)),
            "expected Integer(3), got {result:?}"
        );
    }

    #[test]
    fn subtract_two_numbers() {
        let result = eval_str("(- 10 3)").unwrap();
        assert!(matches!(&result, template::Value::Integer(7)));
    }

    #[test]
    fn multiply_two_numbers() {
        let result = eval_str("(* 4 5)").unwrap();
        assert!(matches!(&result, template::Value::Integer(20)));
    }

    #[test]
    fn negate_single_number() {
        let result = eval_str("(- 5)").unwrap();
        assert!(matches!(&result, template::Value::Integer(-5)));
    }

    #[test]
    fn divide_numbers() {
        let result = eval_str("(/ 10 2)").unwrap();
        assert!(matches!(&result, template::Value::Integer(5)));
    }

    #[test]
    fn modulo() {
        let result = eval_str("(mod 10 3)").unwrap();
        assert!(matches!(&result, template::Value::Integer(1)));
    }

    #[test]
    fn equality_true() {
        let result = eval_str("(= 1 1)").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn equality_false() {
        let result = eval_str("(= 1 2)").unwrap();
        assert!(matches!(&result, template::Value::Bool(false)));
    }

    #[test]
    fn less_than_true() {
        let result = eval_str("(< 1 2)").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn less_than_false() {
        let result = eval_str("(< 2 1)").unwrap();
        assert!(matches!(&result, template::Value::Bool(false)));
    }

    #[test]
    fn greater_than_true() {
        let result = eval_str("(> 2 1)").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn not_false_is_true() {
        let result = eval_str("(not false)").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn not_true_is_false() {
        let result = eval_str("(not true)").unwrap();
        assert!(matches!(&result, template::Value::Bool(false)));
    }

    #[test]
    fn str_concatenation() {
        let result = eval_str(r#"(str "hello" " " "world")"#).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "hello world"));
    }

    // ── literals ─────────────────────────────────────────────────────────────

    #[test]
    fn string_literal_evaluates_to_text() {
        let result = eval_str(r#""hello""#).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "hello"));
    }

    #[test]
    fn integer_literal_evaluates_to_integer() {
        let result = eval_str("42").unwrap();
        assert!(matches!(&result, template::Value::Integer(42)));
    }

    #[test]
    fn nil_evaluates_to_absent() {
        let result = eval_str("nil").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn empty_list_evaluates_to_absent() {
        let result = eval_str("()").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── threading macros ─────────────────────────────────────────────────────

    #[test]
    fn thread_first_arithmetic() {
        let result = eval_str("(-> 1 (+ 2) (* 3))").unwrap();
        assert!(matches!(&result, template::Value::Integer(9)));
    }

    #[test]
    fn thread_last_take() {
        let result = eval_str("(->> [1 2 3] (take 2))").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    // ── collection operations ────────────────────────────────────────────────

    #[test]
    fn count_vector() {
        let result = eval_str("(count [1 2 3])").unwrap();
        assert!(matches!(&result, template::Value::Integer(3)));
    }

    #[test]
    fn first_vector() {
        let result = eval_str("(first [1 2 3])").unwrap();
        assert!(matches!(&result, template::Value::Integer(1)));
    }

    #[test]
    fn rest_vector() {
        let result = eval_str("(rest [1 2 3])").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    #[test]
    fn last_vector() {
        let result = eval_str("(last [1 2 3])").unwrap();
        assert!(matches!(&result, template::Value::Integer(3)));
    }

    #[test]
    fn reverse_vector() {
        let result = eval_str("(reverse [1 2 3])").unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 3);
            assert!(matches!(&items[0], template::Value::Integer(3)));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn take_from_vector() {
        let result = eval_str("(take 3 [1 2 3 4 5])").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 3));
    }

    #[test]
    fn conj_appends() {
        let result = eval_str("(conj [1 2] 3)").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 3));
    }

    #[test]
    fn cons_prepends() {
        let result = eval_str("(cons 0 [1 2 3])").unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 4);
            assert!(matches!(&items[0], template::Value::Integer(0)));
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn concat_two_lists() {
        let result = eval_str("(concat [1 2] [3 4])").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 4));
    }

    #[test]
    fn nth_by_index() {
        let result = eval_str("(nth [10 20 30] 1)").unwrap();
        assert!(matches!(&result, template::Value::Integer(20)));
    }

    #[test]
    fn empty_check_true() {
        let result = eval_str("(empty? [])").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn empty_check_false() {
        let result = eval_str("(empty? [1])").unwrap();
        assert!(matches!(&result, template::Value::Bool(false)));
    }

    #[test]
    fn range_generates_integers() {
        let result = eval_str("(range 3)").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 3));
    }

    #[test]
    fn repeat_generates_list() {
        let result = eval_str("(repeat 3 42)").unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 3);
            assert!(matches!(&items[0], template::Value::Integer(42)));
        } else {
            panic!("expected list");
        }
    }

    // ── map/record operations ─────────────────────────────────────────────────

    #[test]
    fn assoc_adds_key() {
        let result = eval_str("(assoc {:a 1} :b 2)").unwrap();
        if let template::Value::Record(r) = result {
            assert!(r.resolve(&["b"]).is_some());
        } else {
            panic!("expected record");
        }
    }

    #[test]
    fn dissoc_removes_key() {
        let result = eval_str("(dissoc {:a 1 :b 2} :a)").unwrap();
        if let template::Value::Record(r) = result {
            assert!(r.resolve(&["a"]).is_none());
            assert!(r.resolve(&["b"]).is_some());
        } else {
            panic!("expected record");
        }
    }

    #[test]
    fn merge_records() {
        let result = eval_str("(merge {:a 1} {:b 2})").unwrap();
        if let template::Value::Record(r) = result {
            assert!(r.resolve(&["a"]).is_some());
            assert!(r.resolve(&["b"]).is_some());
        } else {
            panic!("expected record");
        }
    }

    #[test]
    fn keys_returns_keyword_list() {
        let result = eval_str("(keys {:a 1 :b 2})").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    #[test]
    fn get_in_nested() {
        let result = eval_str("(get-in {:a {:b 42}} [:a :b])").unwrap();
        assert!(matches!(&result, template::Value::Integer(42)));
    }

    // ── type operation ────────────────────────────────────────────────────────

    #[test]
    fn type_of_integer() {
        let result = eval_str("(type 42)").unwrap();
        assert!(matches!(&result, template::Value::Keyword { name, .. } if name == "integer"));
    }

    #[test]
    fn type_of_string() {
        let result = eval_str(r#"(type "hello")"#).unwrap();
        assert!(matches!(&result, template::Value::Keyword { name, .. } if name == "string"));
    }

    // ── every? and some ──────────────────────────────────────────────────────

    #[test]
    fn every_with_prim_fn() {
        let result = eval_str("(every? not [])").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    // ── conductor-backed queries ──────────────────────────────────────────────

    #[test]
    fn keyword_evaluates_to_keyword_value() {
        let result = eval_str(":post").unwrap();
        assert!(matches!(&result, template::Value::Keyword { namespace: None, name } if name == "post"));
    }

    #[test]
    fn query_returns_list_of_records() {
        let (_dir, cond) = two_post_conductor();
        let result = eval_with_conductor("(query :post)", &cond).unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    #[test]
    fn list_schemas_returns_list() {
        let (_dir, cond) = two_post_conductor();
        let result = eval_with_conductor("(list-schemas)", &cond).unwrap();
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
        let result = eval_with_conductor("(get-schema :post)", &cond).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s.contains("Post title")));
    }

    #[test]
    fn unknown_function_returns_error() {
        let result = eval_str("(frobnicate 1 2)");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unknown function"));
    }

    #[test]
    fn unbound_symbol_returns_error() {
        let result = eval_str("undefined-sym");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("unbound symbol"));
    }

    #[test]
    fn set_form_returns_error() {
        let result = eval_str("#{1 2}");
        assert!(result.is_err());
    }

    // ── refs-to / refs-from ──────────────────────────────────────────────────

    fn linked_conductor() -> Arc<conductor::Conductor> {
        use schema::Spanned;

        let repo = site_repository::SiteRepository::builder()
            .schema("post", POST_SCHEMA_SRC)
            .build();
        let cond = Arc::new(conductor::Conductor::with_repo(PathBuf::from("/test-site"), repo).unwrap());

        // Build document with a link-expression targeting /author/alice
        let doc_with_link = content::Document {
            preamble: im::vector![],
            body: im::vector![Spanned {
                node: content::ContentElement::LinkExpression {
                    text: content::LinkText::Empty,
                    target: content::LinkTarget::PathRef("/author/alice".to_string()),
                },
                span: schema::Span { start: 0, end: 0 },
            }],
            has_separator: false,
            separator_span: None,
        };

        let meta_with_link = node_store_bridge::content_bridge::DocumentMeta {
            url: "/post/with-link".to_string(),
            stem: "post".to_string(),
            file: "content/post/with-link.md".to_string(),
            page_kind: "item".to_string(),
        };

        let doc_no_link = content::Document {
            preamble: im::vector![],
            body: im::vector![Spanned {
                node: content::ContentElement::Paragraph { text: "No links here.".to_string() },
                span: schema::Span { start: 0, end: 0 },
            }],
            has_separator: false,
            separator_span: None,
        };

        let meta_no_link = node_store_bridge::content_bridge::DocumentMeta {
            url: "/post/no-link".to_string(),
            stem: "post".to_string(),
            file: "content/post/no-link.md".to_string(),
            page_kind: "item".to_string(),
        };

        {
            let node_store_arc = cond.node_store();
            let mut store = node_store_arc.write().unwrap();
            let root_with_link = node_store_bridge::content_bridge::document_to_store(
                &doc_with_link, &mut store, Some(&meta_with_link),
            );
            let root_no_link = node_store_bridge::content_bridge::document_to_store(
                &doc_no_link, &mut store, Some(&meta_no_link),
            );
            drop(store);

            cond.insert_url_root("/post/with-link", root_with_link);
            cond.insert_url_root("/post/no-link", root_no_link);
        }

        cond
    }

    #[test]
    fn refs_to_returns_edges_pointing_at_target() {
        let cond = linked_conductor();
        let result = eval_with_conductor(r#"(refs-to "/author/alice")"#, &cond).unwrap();
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
        let result = eval_with_conductor(r#"(refs-to "/author/nobody")"#, &cond).unwrap();
        assert!(
            matches!(result, template::Value::List(ref v) if v.is_empty()),
            "expected empty list for unknown target"
        );
    }

    #[test]
    fn refs_from_returns_edges_from_source() {
        let cond = linked_conductor();
        let result = eval_with_conductor(r#"(refs-from "/post/with-link")"#, &cond).unwrap();
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
        let result = eval_with_conductor(r#"(refs-from "/post/no-link")"#, &cond).unwrap();
        assert!(
            matches!(result, template::Value::List(ref v) if v.is_empty()),
            "expected empty list for page with no links"
        );
    }

    #[test]
    fn refs_to_requires_argument() {
        let cond = empty_conductor();
        let result = eval_with_conductor("(refs-to)", &cond);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("refs-to requires 1 argument"));
    }

    #[test]
    fn refs_from_requires_argument() {
        let cond = empty_conductor();
        let result = eval_with_conductor("(refs-from)", &cond);
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
        let result = eval_str("(let [x 1] x)").unwrap();
        assert!(matches!(result, template::Value::Integer(1)));
    }

    #[test]
    fn let_multiple_bindings() {
        let result = eval_str("(let [x 1 y 2] (+ x y))").unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn let_sequential_bindings() {
        let result = eval_str("(let [x 5 y (+ x 1)] y)").unwrap();
        assert!(matches!(result, template::Value::Integer(6)));
    }

    #[test]
    fn let_multi_body_returns_last() {
        let result = eval_str("(let [x 1] x x 42)").unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    // ── if ──────────────────────────────────────────────────────────────────

    #[test]
    fn if_true_branch() {
        let result = eval_str("(if true 1 2)").unwrap();
        assert!(matches!(result, template::Value::Integer(1)));
    }

    #[test]
    fn if_false_branch() {
        let result = eval_str("(if false 1 2)").unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn if_nil_is_falsy() {
        let result = eval_str("(if nil 1 2)").unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn if_zero_is_truthy() {
        let result = eval_str("(if 0 1 2)").unwrap();
        assert!(matches!(result, template::Value::Integer(1)));
    }

    #[test]
    fn if_no_else_returns_absent() {
        let result = eval_str("(if false 1)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── do ──────────────────────────────────────────────────────────────────

    #[test]
    fn do_returns_last_value() {
        let result = eval_str("(do 1 2 3)").unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn do_empty_returns_absent() {
        let result = eval_str("(do)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── fn and closures ──────────────────────────────────────────────────────

    #[test]
    fn fn_immediate_application() {
        let result = eval_str("((fn [x] (+ x 1)) 10)").unwrap();
        assert!(matches!(result, template::Value::Integer(11)));
    }

    #[test]
    fn fn_captures_lexical_scope() {
        let result = eval_str("(let [n 5] ((fn [x] (+ x n)) 3))").unwrap();
        assert!(matches!(result, template::Value::Integer(8)));
    }

    #[test]
    fn fn_stored_in_let() {
        let result = eval_str("(let [f (fn [x] (* x 2))] (f 5))").unwrap();
        assert!(matches!(result, template::Value::Integer(10)));
    }

    #[test]
    fn fn_variadic_rest_param() {
        let result = eval_str("((fn [x & rest] rest) 1 2 3)").unwrap();
        assert!(
            matches!(&result, template::Value::List(items) if items.len() == 2),
            "expected List of 2, got {result:?}"
        );
    }

    #[test]
    fn fn_produces_fn_value() {
        let result = eval_str("(fn [x] x)").unwrap();
        assert!(matches!(result, template::Value::Fn(_)));
    }

    // ── def ─────────────────────────────────────────────────────────────────

    #[test]
    fn def_in_do_block() {
        let result = eval_str("(do (def x 42) x)").unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    // ── defn macro ───────────────────────────────────────────────────────────

    #[test]
    fn defn_defines_and_calls_function() {
        let result = eval_str("(do (defn double [x] (* x 2)) (double 5))").unwrap();
        assert!(matches!(result, template::Value::Integer(10)));
    }

    // ── quote ────────────────────────────────────────────────────────────────

    #[test]
    fn quote_returns_form_as_data() {
        let result = eval_str("(quote (+ 1 2))").unwrap();
        assert!(
            matches!(&result, template::Value::List(items) if items.len() == 3),
            "expected List of 3, got {result:?}"
        );
    }

    #[test]
    fn quote_keyword() {
        let result = eval_str("(quote :foo)").unwrap();
        assert!(
            matches!(&result, template::Value::Keyword { name, .. } if name == "foo"),
            "got {result:?}"
        );
    }

    // ── Value::Integer, Value::Bool, Value::Keyword ──────────────────────────

    #[test]
    fn bool_literal_true() {
        let result = eval_str("true").unwrap();
        assert!(matches!(result, template::Value::Bool(true)));
    }

    #[test]
    fn bool_literal_false() {
        let result = eval_str("false").unwrap();
        assert!(matches!(result, template::Value::Bool(false)));
    }

    // ── macro expansions (when, cond, and, or) ───────────────────────────────

    #[test]
    fn when_true_evaluates_body() {
        let result = eval_str("(when true 42)").unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn when_false_returns_nil() {
        let result = eval_str("(when false 42)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn when_not_true_returns_nil() {
        let result = eval_str("(when-not true 42)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn when_not_false_evaluates_body() {
        let result = eval_str("(when-not false 42)").unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn cond_first_matching_clause() {
        let result = eval_str("(cond false 1 true 2 true 3)").unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn cond_no_match_returns_nil() {
        let result = eval_str("(cond false 1 false 2)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn and_all_true() {
        let result = eval_str("(and 1 2 3)").unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn and_short_circuits_on_false() {
        let result = eval_str("(and 1 false 3)").unwrap();
        assert!(matches!(result, template::Value::Bool(false)));
    }

    #[test]
    fn and_empty_returns_true() {
        let result = eval_str("(and)").unwrap();
        assert!(matches!(result, template::Value::Bool(true)));
    }

    #[test]
    fn or_first_truthy() {
        let result = eval_str("(or false nil 42)").unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn or_all_false_returns_nil() {
        let result = eval_str("(or false nil)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    #[test]
    fn or_empty_returns_nil() {
        let result = eval_str("(or)").unwrap();
        assert!(matches!(result, template::Value::Absent));
    }

    // ── map with fn ──────────────────────────────────────────────────────────

    #[test]
    fn map_with_fn_value() {
        // (map (fn [x] (+ x 1)) [1 2 3]) → [2 3 4]
        let result = eval_str("(map (fn [x] (+ x 1)) [1 2 3])").unwrap();
        if let template::Value::List(items) = result {
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], template::Value::Integer(2)));
            assert!(matches!(items[1], template::Value::Integer(3)));
            assert!(matches!(items[2], template::Value::Integer(4)));
        } else {
            panic!("expected list");
        }
    }

    // ── reduce ───────────────────────────────────────────────────────────────

    #[test]
    fn reduce_sum() {
        let result = eval_str("(reduce + 0 [1 2 3 4 5])").unwrap();
        assert!(matches!(result, template::Value::Integer(15)));
    }

    #[test]
    fn reduce_without_init() {
        let result = eval_str("(reduce + [1 2 3])").unwrap();
        assert!(matches!(result, template::Value::Integer(6)));
    }

    // ── Phase 4: registered primitives ───────────────────────────────────────

    #[test]
    fn plus_registered_in_root() {
        // Verify + is accessible as a value (can pass as argument)
        let result = eval_str("(let [f +] (f 3 4))").unwrap();
        assert!(matches!(result, template::Value::Integer(7)));
    }

    #[test]
    fn apply_with_prim_fn() {
        let result = eval_str("(apply + [1 2 3])").unwrap();
        assert!(matches!(result, template::Value::Integer(6)));
    }

    #[test]
    fn select_keys_filters() {
        let result = eval_str("(select-keys {:a 1 :b 2 :c 3} [:a :c])").unwrap();
        if let template::Value::Record(r) = result {
            assert!(r.resolve(&["a"]).is_some());
            assert!(r.resolve(&["b"]).is_none());
            assert!(r.resolve(&["c"]).is_some());
        } else {
            panic!("expected record");
        }
    }

    #[test]
    fn keyword_fn_creates_keyword() {
        let result = eval_str(r#"(keyword "foo")"#).unwrap();
        assert!(matches!(&result, template::Value::Keyword { name, .. } if name == "foo"));
    }

    #[test]
    fn name_fn_extracts_name() {
        let result = eval_str("(name :hello)").unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "hello"));
    }

    #[test]
    fn contains_check() {
        let result = eval_str("(contains? {:a 1} :a)").unwrap();
        assert!(matches!(&result, template::Value::Bool(true)));
    }

    #[test]
    fn filter_with_pred_fn() {
        let result = eval_str(r#"(filter :title "Alpha Post" [{:title "Alpha Post"} {:title "Beta Post"}])"#).unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 1));
    }

    // ── Phase 5: prelude tests ────────────────────────────────────────────────

    #[test]
    fn prelude_identity() {
        let result = eval_str("(identity 42)").unwrap();
        assert!(matches!(result, template::Value::Integer(42)));
    }

    #[test]
    fn prelude_inc_dec() {
        assert!(matches!(eval_str("(inc 5)").unwrap(), template::Value::Integer(6)));
        assert!(matches!(eval_str("(dec 5)").unwrap(), template::Value::Integer(4)));
    }

    #[test]
    fn prelude_predicates() {
        assert!(matches!(eval_str("(nil? nil)").unwrap(), template::Value::Bool(true)));
        assert!(matches!(eval_str("(nil? 1)").unwrap(), template::Value::Bool(false)));
        assert!(matches!(eval_str("(zero? 0)").unwrap(), template::Value::Bool(true)));
        assert!(matches!(eval_str("(even? 4)").unwrap(), template::Value::Bool(true)));
        assert!(matches!(eval_str("(odd? 3)").unwrap(), template::Value::Bool(true)));
    }

    #[test]
    fn prelude_comp() {
        let result = eval_str("((comp inc inc) 1)").unwrap();
        assert!(matches!(result, template::Value::Integer(3)));
    }

    #[test]
    fn prelude_complement() {
        let result = eval_str("((complement zero?) 0)").unwrap();
        assert!(matches!(result, template::Value::Bool(false)));
    }

    #[test]
    fn prelude_second() {
        let result = eval_str("(second [1 2 3])").unwrap();
        assert!(matches!(result, template::Value::Integer(2)));
    }

    #[test]
    fn prelude_drop() {
        let result = eval_str("(drop 2 [1 2 3 4])").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 2));
    }

    #[test]
    fn prelude_str_join() {
        let result = eval_str(r#"(str-join ", " ["a" "b" "c"])"#).unwrap();
        assert!(matches!(&result, template::Value::Text(s) if s == "a, b, c"));
    }

    #[test]
    fn prelude_distinct() {
        let result = eval_str("(distinct [1 2 1 3 2])").unwrap();
        assert!(matches!(&result, template::Value::List(items) if items.len() == 3));
    }

    #[test]
    fn prelude_max_min() {
        assert!(matches!(eval_str("(max 3 7)").unwrap(), template::Value::Integer(7)));
        assert!(matches!(eval_str("(min 3 7)").unwrap(), template::Value::Integer(3)));
    }

    // ── DocRegistry and doc special form tests (ADR-039 Phase A) ─────────────

    #[test]
    fn doc_primitive_shows_documentation() {
        // (doc +) should return text containing the name and doc
        let result = eval_str("(doc +)").unwrap();
        if let template::Value::Text(s) = result {
            assert!(s.contains('+'), "doc output should contain the symbol name");
            assert!(s.contains("Add numbers"), "doc output should contain the description");
        } else {
            panic!("expected Text from (doc +), got {result:?}");
        }
    }

    #[test]
    fn doc_prelude_function_shows_documentation() {
        // (doc identity) should return text with the docstring
        let result = eval_str("(doc identity)").unwrap();
        if let template::Value::Text(s) = result {
            assert!(s.contains("identity"), "doc output should contain 'identity'");
            assert!(s.contains("unchanged") || s.contains("argument"), "doc should contain the docstring");
        } else {
            panic!("expected Text from (doc identity), got {result:?}");
        }
    }

    #[test]
    fn doc_no_args_lists_all_functions() {
        let result = eval_str("(doc)").unwrap();
        if let template::Value::Text(s) = result {
            assert!(s.contains("Primitives"), "doc() should list Primitives section");
            assert!(s.contains("Core library"), "doc() should list Core library section");
            assert!(s.contains('+'), "doc() should list '+' primitive");
            assert!(s.contains("identity"), "doc() should list 'identity' from prelude");
        } else {
            panic!("expected Text from (doc), got {result:?}");
        }
    }

    #[test]
    fn doc_threading_macro_shows_documentation() {
        let result = eval_str("(doc ->)").unwrap();
        if let template::Value::Text(s) = result {
            assert!(s.contains("->") || s.contains("Thread"), "doc should mention threading");
        } else {
            panic!("expected Text from (doc ->), got {result:?}");
        }
    }

    #[test]
    fn defn_with_docstring_registers_in_doc_registry() {
        // Define a function with a docstring, then doc it
        let result = eval_str(
            r#"(do (defn my-fn "does the thing" [x] x) (doc my-fn))"#,
        ).unwrap();
        if let template::Value::Text(s) = result {
            assert!(s.contains("my-fn"), "doc should contain function name");
            assert!(s.contains("does the thing"), "doc should contain the docstring");
        } else {
            panic!("expected Text from (doc my-fn), got {result:?}");
        }
    }

    #[test]
    fn doc_registry_completions_prefix() {
        // Test the DocRegistry completions method directly
        let registry = crate::doc_registry::DocRegistry::new();
        registry.register(crate::doc_registry::DocEntry {
            name: "map".to_string(),
            doc: "Apply f to each item.".to_string(),
            arglists: vec!["(map f coll)".to_string()],
            source: crate::doc_registry::DocSource::Primitive,
        });
        registry.register(crate::doc_registry::DocEntry {
            name: "mapcat".to_string(),
            doc: "Map and concatenate results.".to_string(),
            arglists: vec!["(mapcat f coll)".to_string()],
            source: crate::doc_registry::DocSource::Prelude,
        });
        registry.register(crate::doc_registry::DocEntry {
            name: "max".to_string(),
            doc: "Return the larger of two values.".to_string(),
            arglists: vec!["(max a b)".to_string()],
            source: crate::doc_registry::DocSource::Prelude,
        });
        registry.register(crate::doc_registry::DocEntry {
            name: "filter".to_string(),
            doc: "Filter a collection.".to_string(),
            arglists: vec!["(filter pred coll)".to_string()],
            source: crate::doc_registry::DocSource::Primitive,
        });

        let matches = registry.completions("ma");
        let names: Vec<&str> = matches.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"map"), "completions('ma') should include 'map'");
        assert!(names.contains(&"mapcat"), "completions('ma') should include 'mapcat'");
        assert!(names.contains(&"max"), "completions('ma') should include 'max'");
        assert!(!names.contains(&"filter"), "completions('ma') should not include 'filter'");
    }

    #[test]
    fn doc_unknown_symbol_returns_error() {
        let result = eval_str("(doc totally-unknown-fn)");
        assert!(result.is_err(), "doc of unknown symbol should return error");
        assert!(
            result.unwrap_err().contains("no documentation for"),
            "error should mention 'no documentation for'"
        );
    }
}
