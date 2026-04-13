//! Pure primitive functions registered in the root environment at startup.
//!
//! These functions take `Vec<Value>` (pre-evaluated arguments) and do not need
//! a conductor reference. Higher-order functions that require invoking user
//! closures (map, filter, reduce, sort-by) remain in the legacy match dispatch
//! in `lib.rs` until a conductor-aware invocation mechanism is available.

use std::sync::Arc;
use template::Value;
use crate::closure::PrimitiveFn;
use crate::env::RootEnv;

// ---------------------------------------------------------------------------
// Registration entry point
// ---------------------------------------------------------------------------

/// Register all pure primitive functions into the root environment.
/// Called once at the start of each `eval` call.
pub fn register_builtins(root: &RootEnv) {
    // ── Arithmetic ───────────────────────────────────────────────────────────
    root.def("+", prim("+", |args| {
        let mut sum: i64 = 0;
        for a in &args {
            sum += value_to_i64(a, "+")?;
        }
        Ok(Value::Integer(sum))
    }));

    root.def("-", prim("-", |args| {
        if args.is_empty() {
            return Err("- requires at least 1 argument".into());
        }
        let mut result = value_to_i64(&args[0], "-")?;
        if args.len() == 1 {
            return Ok(Value::Integer(-result));
        }
        for a in &args[1..] {
            result -= value_to_i64(a, "-")?;
        }
        Ok(Value::Integer(result))
    }));

    root.def("*", prim("*", |args| {
        let mut product: i64 = 1;
        for a in &args {
            product *= value_to_i64(a, "*")?;
        }
        Ok(Value::Integer(product))
    }));

    root.def("/", prim("/", |args| {
        if args.len() < 2 {
            return Err("/ requires at least 2 arguments".into());
        }
        let mut result = value_to_i64(&args[0], "/")?;
        for a in &args[1..] {
            let n = value_to_i64(a, "/")?;
            if n == 0 {
                return Err("/ division by zero".into());
            }
            result /= n;
        }
        Ok(Value::Integer(result))
    }));

    root.def("mod", prim("mod", |args| {
        if args.len() != 2 {
            return Err("mod requires exactly 2 arguments".into());
        }
        let n = value_to_i64(&args[0], "mod")?;
        let d = value_to_i64(&args[1], "mod")?;
        if d == 0 {
            return Err("mod division by zero".into());
        }
        Ok(Value::Integer(n % d))
    }));

    // ── Comparison ───────────────────────────────────────────────────────────
    root.def("=", prim("=", |args| {
        if args.len() < 2 {
            return Err("= requires at least 2 arguments".into());
        }
        let first = value_to_string(&args[0]);
        for a in &args[1..] {
            if first != value_to_string(a) {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(true))
    }));

    root.def("<", prim("<", |args| {
        if args.len() < 2 {
            return Err("< requires at least 2 arguments".into());
        }
        let mut prev = value_to_i64(&args[0], "<")?;
        for a in &args[1..] {
            let cur = value_to_i64(a, "<")?;
            if prev >= cur {
                return Ok(Value::Bool(false));
            }
            prev = cur;
        }
        Ok(Value::Bool(true))
    }));

    root.def(">", prim(">", |args| {
        if args.len() < 2 {
            return Err("> requires at least 2 arguments".into());
        }
        let mut prev = value_to_i64(&args[0], ">")?;
        for a in &args[1..] {
            let cur = value_to_i64(a, ">")?;
            if prev <= cur {
                return Ok(Value::Bool(false));
            }
            prev = cur;
        }
        Ok(Value::Bool(true))
    }));

    root.def("not", prim("not", |args| {
        if args.len() != 1 {
            return Err("not requires exactly 1 argument".into());
        }
        let falsy = matches!(&args[0], Value::Bool(false) | Value::Absent);
        Ok(Value::Bool(falsy))
    }));

    // ── Collection operations ─────────────────────────────────────────────────
    root.def("first", prim("first", |args| {
        if args.len() != 1 {
            return Err("first requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => Ok(items.first().cloned().unwrap_or(Value::Absent)),
            Value::Text(s) => Ok(s.chars().next()
                .map(|c| Value::Text(c.to_string()))
                .unwrap_or(Value::Absent)),
            _ => Err("first expects a list or string".into()),
        }
    }));

    root.def("rest", prim("rest", |args| {
        if args.len() != 1 {
            return Err("rest requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => {
                if items.is_empty() {
                    Ok(Value::List(vec![]))
                } else {
                    Ok(Value::List(items[1..].to_vec()))
                }
            }
            _ => Err("rest expects a list".into()),
        }
    }));

    root.def("last", prim("last", |args| {
        if args.len() != 1 {
            return Err("last requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => Ok(items.last().cloned().unwrap_or(Value::Absent)),
            _ => Err("last expects a list".into()),
        }
    }));

    root.def("count", prim("count", |args| {
        if args.len() != 1 {
            return Err("count requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => Ok(Value::Integer(items.len() as i64)),
            Value::Text(s) => Ok(Value::Integer(s.len() as i64)),
            Value::Absent => Ok(Value::Integer(0)),
            _ => Err("count expects a list or string".into()),
        }
    }));

    root.def("reverse", prim("reverse", |args| {
        if args.len() != 1 {
            return Err("reverse requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => {
                let mut rev = items.clone();
                rev.reverse();
                Ok(Value::List(rev))
            }
            _ => Err("reverse expects a list".into()),
        }
    }));

    root.def("take", prim("take", |args| {
        if args.len() != 2 {
            return Err("take requires 2 arguments: count and collection".into());
        }
        let n = value_to_i64(&args[0], "take")? as usize;
        match &args[1] {
            Value::List(items) => Ok(Value::List(items.iter().take(n).cloned().collect())),
            _ => Err("take expects a list as second argument".into()),
        }
    }));

    root.def("nth", prim("nth", |args| {
        if args.len() < 2 {
            return Err("nth requires at least 2 arguments: coll and index".into());
        }
        let idx = value_to_i64(&args[1], "nth")?;
        match &args[0] {
            Value::List(items) => {
                if idx < 0 || idx as usize >= items.len() {
                    if args.len() > 2 {
                        Ok(args[2].clone())
                    } else {
                        Err(format!("nth index {idx} out of bounds (len {})", items.len()))
                    }
                } else {
                    Ok(items[idx as usize].clone())
                }
            }
            _ => Err("nth expects a list".into()),
        }
    }));

    root.def("conj", prim("conj", |args| {
        if args.len() < 2 {
            return Err("conj requires at least 2 arguments: coll and item".into());
        }
        match &args[0] {
            Value::List(items) => {
                let mut result = items.clone();
                for a in &args[1..] {
                    result.push(a.clone());
                }
                Ok(Value::List(result))
            }
            Value::Absent => {
                Ok(Value::List(args[1..].to_vec()))
            }
            _ => Err("conj expects a list or nil".into()),
        }
    }));

    root.def("cons", prim("cons", |args| {
        if args.len() != 2 {
            return Err("cons requires 2 arguments: item and coll".into());
        }
        match &args[1] {
            Value::List(items) => {
                let mut result = vec![args[0].clone()];
                result.extend_from_slice(items);
                Ok(Value::List(result))
            }
            Value::Absent => Ok(Value::List(vec![args[0].clone()])),
            _ => Err("cons expects a list as second argument".into()),
        }
    }));

    root.def("concat", prim("concat", |args| {
        let mut result = Vec::new();
        for a in &args {
            match a {
                Value::List(items) => result.extend_from_slice(items),
                Value::Absent => {}
                _ => return Err("concat expects lists".into()),
            }
        }
        Ok(Value::List(result))
    }));

    root.def("empty?", prim("empty?", |args| {
        if args.len() != 1 {
            return Err("empty? requires 1 argument".into());
        }
        let empty = match &args[0] {
            Value::List(items) => items.is_empty(),
            Value::Text(s) => s.is_empty(),
            Value::Absent => true,
            _ => false,
        };
        Ok(Value::Bool(empty))
    }));

    // NOTE: contains? is in legacy dispatch (lib.rs) because keyword args evaluate
    // to stem queries when unnamespaced (e.g., `:a` → `List([])`)

    root.def("vec", prim("vec", |args| {
        if args.len() != 1 {
            return Err("vec requires 1 argument".into());
        }
        match &args[0] {
            Value::List(_) => Ok(args[0].clone()),
            Value::Absent => Ok(Value::List(vec![])),
            _ => Ok(Value::List(vec![args[0].clone()])),
        }
    }));

    root.def("range", prim("range", |args| {
        match args.len() {
            1 => {
                let end = value_to_i64(&args[0], "range")?;
                Ok(Value::List((0..end).map(Value::Integer).collect()))
            }
            2 => {
                let start = value_to_i64(&args[0], "range")?;
                let end = value_to_i64(&args[1], "range")?;
                Ok(Value::List((start..end).map(Value::Integer).collect()))
            }
            3 => {
                let start = value_to_i64(&args[0], "range")?;
                let end = value_to_i64(&args[1], "range")?;
                let step = value_to_i64(&args[2], "range")?;
                if step == 0 {
                    return Err("range step cannot be zero".into());
                }
                let mut result = Vec::new();
                let mut i = start;
                while if step > 0 { i < end } else { i > end } {
                    result.push(Value::Integer(i));
                    i += step;
                }
                Ok(Value::List(result))
            }
            _ => Err("range requires 1, 2, or 3 arguments".into()),
        }
    }));

    root.def("repeat", prim("repeat", |args| {
        if args.len() != 2 {
            return Err("repeat requires 2 arguments: n and value".into());
        }
        let n = value_to_i64(&args[0], "repeat")? as usize;
        Ok(Value::List(vec![args[1].clone(); n]))
    }));

    // ── Map/record operations ─────────────────────────────────────────────────
    // NOTE: get, get-in, assoc, dissoc are in legacy dispatch (lib.rs) because
    // keyword args (e.g., `:key`) evaluate to stem queries when unnamespaced.

    root.def("merge", prim("merge", |args| {
        let mut result = template::DataGraph::new();
        for a in &args {
            match a {
                Value::Record(r) => {
                    for (k, v) in r.iter() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                Value::Absent => {} // merge ignores nil
                _ => return Err("merge expects maps".into()),
            }
        }
        Ok(Value::Record(result))
    }));

    // NOTE: select-keys is in legacy dispatch (lib.rs) because keyword args
    // evaluate to stem queries when unnamespaced.

    root.def("keys", prim("keys", |args| {
        if args.len() != 1 {
            return Err("keys requires 1 argument".into());
        }
        match &args[0] {
            Value::Record(r) => {
                let keys: Vec<Value> = r.iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(k, _)| Value::Keyword { namespace: None, name: k.clone() })
                    .collect();
                Ok(Value::List(keys))
            }
            _ => Err("keys expects a map".into()),
        }
    }));

    root.def("vals", prim("vals", |args| {
        if args.len() != 1 {
            return Err("vals requires 1 argument".into());
        }
        match &args[0] {
            Value::Record(r) => {
                let vals: Vec<Value> = r.iter()
                    .filter(|(k, _)| !k.starts_with('_'))
                    .map(|(_, v)| v.clone())
                    .collect();
                Ok(Value::List(vals))
            }
            _ => Err("vals expects a map".into()),
        }
    }));

    // ── String operations ─────────────────────────────────────────────────────
    root.def("str", prim("str", |args| {
        let mut result = String::new();
        for a in &args {
            result.push_str(&value_to_string(a));
        }
        Ok(Value::Text(result))
    }));

    root.def("subs", prim("subs", |args| {
        if args.len() < 2 {
            return Err("subs requires at least 2 arguments: string, start".into());
        }
        let s = match &args[0] {
            Value::Text(t) => t.clone(),
            _ => return Err("subs expects a string".into()),
        };
        let start = value_to_i64(&args[1], "subs")? as usize;
        if args.len() == 2 {
            Ok(Value::Text(s.chars().skip(start).collect()))
        } else {
            let end = value_to_i64(&args[2], "subs")? as usize;
            Ok(Value::Text(s.chars().skip(start).take(end - start).collect()))
        }
    }));

    // NOTE: name is in legacy dispatch (lib.rs) because keyword args evaluate
    // to stem queries when unnamespaced (e.g., `:foo` → `List([])`).

    root.def("keyword", prim("keyword", |args| {
        if args.is_empty() || args.len() > 2 {
            return Err("keyword requires 1 or 2 arguments".into());
        }
        if args.len() == 1 {
            let name = match &args[0] {
                Value::Text(s) => s.clone(),
                Value::Keyword { name, .. } => name.clone(),
                _ => return Err("keyword expects a string".into()),
            };
            Ok(Value::Keyword { namespace: None, name })
        } else {
            let namespace = match &args[0] {
                Value::Text(s) => s.clone(),
                _ => return Err("keyword namespace must be a string".into()),
            };
            let name = match &args[1] {
                Value::Text(s) => s.clone(),
                _ => return Err("keyword name must be a string".into()),
            };
            Ok(Value::Keyword { namespace: Some(namespace), name })
        }
    }));

    // ── Type operations ───────────────────────────────────────────────────────
    root.def("type", prim("type", |args| {
        if args.len() != 1 {
            return Err("type requires 1 argument".into());
        }
        let kw = match &args[0] {
            Value::Integer(_) => "integer",
            Value::Bool(_) => "boolean",
            Value::Text(_) => "string",
            Value::Keyword { .. } => "keyword",
            Value::List(_) => "list",
            Value::Record(_) => "map",
            Value::Fn(_) => "fn",
            Value::Absent => "nil",
            Value::Html(_) => "string",
            Value::Suggestion { .. } => "nil",
            Value::LinkExpression { .. } => "nil",
        };
        Ok(Value::Keyword { namespace: None, name: kw.to_string() })
    }));

    // NOTE: apply, every?, some, map, filter, reduce, sort-by are in the
    // conductor-aware legacy dispatch in lib.rs because they need to invoke
    // user-defined closures (Closure::apply requires a conductor reference).
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a `Value::Fn` backed by the given pure function.
fn prim(name: &str, func: impl Fn(Vec<Value>) -> Result<Value, String> + Send + Sync + 'static) -> Value {
    Value::Fn(Arc::new(PrimitiveFn::new(name, func)))
}

/// Extract an i64 from a Value.
pub(crate) fn value_to_i64(v: &Value, op: &str) -> Result<i64, String> {
    match v {
        Value::Integer(n) => Ok(*n),
        Value::Text(s) => s.parse::<i64>().map_err(|_| format!("{op}: not a number: {s}")),
        other => Err(format!("{op}: expected a number, got: {other:?}")),
    }
}

/// Convert a Value to a plain string for comparison / display.
pub(crate) fn value_to_string(v: &Value) -> String {
    match v.display_text() {
        Some(s) => s,
        None => edn::value_to_edn(v),
    }
}
