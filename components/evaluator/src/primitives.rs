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
use crate::doc_registry::{DocEntry, DocSource};

// ---------------------------------------------------------------------------
// Registration entry point
// ---------------------------------------------------------------------------

/// Register all pure primitive functions into the root environment.
/// Called once at the start of each `eval` call.
pub fn register_builtins(root: &RootEnv) {
    // ── Arithmetic ───────────────────────────────────────────────────────────
    prim_doc(root, "+", "(+ a b ...)", "Add numbers.", |args| {
        let mut sum: i64 = 0;
        for a in &args {
            sum += value_to_i64(a, "+")?;
        }
        Ok(Value::Integer(sum))
    });

    prim_doc(root, "-", "(- a b ...) or (- a)", "Subtract numbers. With one arg, negates.", |args| {
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
    });

    prim_doc(root, "*", "(* a b ...)", "Multiply numbers.", |args| {
        let mut product: i64 = 1;
        for a in &args {
            product *= value_to_i64(a, "*")?;
        }
        Ok(Value::Integer(product))
    });

    prim_doc(root, "/", "(/ a b ...)", "Divide numbers (integer division).", |args| {
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
    });

    prim_doc(root, "mod", "(mod a b)", "Modulo — remainder of a divided by b.", |args| {
        if args.len() != 2 {
            return Err("mod requires exactly 2 arguments".into());
        }
        let n = value_to_i64(&args[0], "mod")?;
        let d = value_to_i64(&args[1], "mod")?;
        if d == 0 {
            return Err("mod division by zero".into());
        }
        Ok(Value::Integer(n % d))
    });

    // ── Comparison ───────────────────────────────────────────────────────────
    prim_doc(root, "=", "(= a b ...)", "Check equality of two or more values.", |args| {
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
    });

    prim_doc(root, "<", "(< a b ...)", "Return true if arguments are in strictly increasing order.", |args| {
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
    });

    prim_doc(root, ">", "(> a b ...)", "Return true if arguments are in strictly decreasing order.", |args| {
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
    });

    prim_doc(root, "not", "(not x)", "Logical negation — returns true if x is false or nil.", |args| {
        if args.len() != 1 {
            return Err("not requires exactly 1 argument".into());
        }
        let falsy = matches!(&args[0], Value::Bool(false) | Value::Absent);
        Ok(Value::Bool(falsy))
    });

    // ── Collection operations ─────────────────────────────────────────────────
    prim_doc(root, "first", "(first coll)", "Return the first item of a collection, or nil if empty.", |args| {
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
    });

    prim_doc(root, "rest", "(rest coll)", "Return all items except the first, or [] if empty.", |args| {
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
    });

    prim_doc(root, "last", "(last coll)", "Return the last item of a collection, or nil if empty.", |args| {
        if args.len() != 1 {
            return Err("last requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => Ok(items.last().cloned().unwrap_or(Value::Absent)),
            _ => Err("last expects a list".into()),
        }
    });

    prim_doc(root, "count", "(count coll)", "Return the number of items in a collection or string.", |args| {
        if args.len() != 1 {
            return Err("count requires 1 argument".into());
        }
        match &args[0] {
            Value::List(items) => Ok(Value::Integer(items.len() as i64)),
            Value::Text(s) => Ok(Value::Integer(s.len() as i64)),
            Value::Absent => Ok(Value::Integer(0)),
            _ => Err("count expects a list or string".into()),
        }
    });

    prim_doc(root, "reverse", "(reverse coll)", "Return a collection with items in reversed order.", |args| {
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
    });

    prim_doc(root, "take", "(take n coll)", "Take the first n items from a collection.", |args| {
        if args.len() != 2 {
            return Err("take requires 2 arguments: count and collection".into());
        }
        let n = value_to_i64(&args[0], "take")? as usize;
        match &args[1] {
            Value::List(items) => Ok(Value::List(items.iter().take(n).cloned().collect())),
            _ => Err("take expects a list as second argument".into()),
        }
    });

    prim_doc(root, "nth", "(nth coll n) or (nth coll n default)", "Get item at index n (0-based). Returns default or errors if out of bounds.", |args| {
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
    });

    prim_doc(root, "conj", "(conj coll item ...)", "Append one or more items to a collection.", |args| {
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
    });

    prim_doc(root, "cons", "(cons item coll)", "Prepend an item to a collection.", |args| {
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
    });

    prim_doc(root, "concat", "(concat coll ...)", "Concatenate collections into a single list.", |args| {
        let mut result = Vec::new();
        for a in &args {
            match a {
                Value::List(items) => result.extend_from_slice(items),
                Value::Absent => {}
                _ => return Err("concat expects lists".into()),
            }
        }
        Ok(Value::List(result))
    });

    prim_doc(root, "empty?", "(empty? coll)", "Return true if the collection or string is empty.", |args| {
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
    });

    // NOTE: contains? is in legacy dispatch (lib.rs) because keyword args evaluate
    // to stem queries when unnamespaced (e.g., `:a` → `List([])`)

    prim_doc(root, "vec", "(vec x)", "Convert a value to a vector/list.", |args| {
        if args.len() != 1 {
            return Err("vec requires 1 argument".into());
        }
        match &args[0] {
            Value::List(_) => Ok(args[0].clone()),
            Value::Absent => Ok(Value::List(vec![])),
            _ => Ok(Value::List(vec![args[0].clone()])),
        }
    });

    prim_doc(root, "range", "(range end) or (range start end) or (range start end step)", "Generate a range of integers.", |args| {
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
    });

    prim_doc(root, "repeat", "(repeat n val)", "Return a list of val repeated n times.", |args| {
        if args.len() != 2 {
            return Err("repeat requires 2 arguments: n and value".into());
        }
        let n = value_to_i64(&args[0], "repeat")? as usize;
        Ok(Value::List(vec![args[1].clone(); n]))
    });

    // ── Map/record operations ─────────────────────────────────────────────────
    // NOTE: get, get-in, assoc, dissoc are in legacy dispatch (lib.rs) because
    // keyword args (e.g., `:key`) evaluate to stem queries when unnamespaced.

    prim_doc(root, "merge", "(merge map1 map2 ...)", "Merge records; later maps override earlier ones.", |args| {
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
    });

    // NOTE: select-keys is in legacy dispatch (lib.rs) because keyword args
    // evaluate to stem queries when unnamespaced.

    prim_doc(root, "keys", "(keys map)", "Return all keys of a record as keywords.", |args| {
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
    });

    prim_doc(root, "vals", "(vals map)", "Return all values of a record.", |args| {
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
    });

    // ── String operations ─────────────────────────────────────────────────────
    prim_doc(root, "str", "(str a b ...)", "Concatenate values as strings.", |args| {
        let mut result = String::new();
        for a in &args {
            result.push_str(&value_to_string(a));
        }
        Ok(Value::Text(result))
    });

    prim_doc(root, "subs", "(subs s start) or (subs s start end)", "Return a substring of s.", |args| {
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
    });

    // NOTE: name is in legacy dispatch (lib.rs) because keyword args evaluate
    // to stem queries when unnamespaced (e.g., `:foo` → `List([])`).

    prim_doc(root, "keyword", "(keyword \"name\") or (keyword \"ns\" \"name\")", "Create a keyword from a string, optionally with a namespace.", |args| {
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
    });

    // ── Type operations ───────────────────────────────────────────────────────
    prim_doc(root, "type", "(type x)", "Return the type of a value as a keyword (:integer, :string, :list, :map, :fn, :boolean, :keyword, or :nil).", |args| {
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
    });

    // NOTE: apply, every?, some, map, filter, reduce, sort-by are in the
    // conductor-aware legacy dispatch in lib.rs because they need to invoke
    // user-defined closures (Closure::apply requires a conductor reference).

    // ── Map/record operations (Phase 6: keywords are now values) ────────────

    prim_doc(root, "get", "(get map :key) or (get map :key default)", "Get a value from a record by keyword, returning default or nil if missing.", |args| {
        if args.len() < 2 {
            return Err("get requires at least 2 arguments: map and key".into());
        }
        let key = keyword_name(&args[1])?;
        let default = args.get(2).cloned().unwrap_or(Value::Absent);
        match &args[0] {
            Value::Record(r) => Ok(r.resolve(&[key.as_str()]).cloned().unwrap_or(default)),
            _ => Ok(default),
        }
    });

    prim_doc(root, "get-in", "(get-in map [:k1 :k2 ...]) or (get-in map keys default)", "Get a nested value from a record by a path of keys.", |args| {
        // (get-in map [:key1 :key2]) — Clojure convention: vector of keys
        if args.len() < 2 {
            return Err("get-in requires at least 2 arguments: map and key-vector".into());
        }
        let keys = match &args[1] {
            Value::List(ks) => ks.clone(),
            _ => return Err("get-in: second argument must be a vector of keys".into()),
        };
        let default = args.get(2).cloned().unwrap_or(Value::Absent);
        let mut current = args[0].clone();
        for k in &keys {
            let key = keyword_name(k)?;
            current = match current {
                Value::Record(ref r) => {
                    r.resolve(&[key.as_str()]).cloned().unwrap_or(Value::Absent)
                }
                _ => Value::Absent,
            };
        }
        if matches!(current, Value::Absent) {
            Ok(default)
        } else {
            Ok(current)
        }
    });

    prim_doc(root, "assoc", "(assoc map :key val ...)", "Associate key-value pairs into a record, returning a new record.", |args| {
        if args.len() < 3 {
            return Err("assoc requires at least 3 arguments: map, key, value".into());
        }
        let mut result = match &args[0] {
            Value::Record(r) => r.clone(),
            Value::Absent => template::DataGraph::new(),
            _ => return Err("assoc expects a map as first argument".into()),
        };
        let mut i = 1;
        while i + 1 < args.len() {
            let key = keyword_name(&args[i])?;
            let val = args[i + 1].clone();
            result.insert(key, val);
            i += 2;
        }
        Ok(Value::Record(result))
    });

    prim_doc(root, "dissoc", "(dissoc map :key ...)", "Remove keys from a record, returning a new record.", |args| {
        if args.len() < 2 {
            return Err("dissoc requires at least 2 arguments: map and key".into());
        }
        let source = match &args[0] {
            Value::Record(r) => r.clone(),
            _ => return Err("dissoc expects a map as first argument".into()),
        };
        let keys_to_remove: Vec<String> = args[1..]
            .iter()
            .map(keyword_name)
            .collect::<Result<_, _>>()?;
        let mut result = template::DataGraph::new();
        for (k, v) in source.iter() {
            if !keys_to_remove.contains(k) {
                result.insert(k.clone(), v.clone());
            }
        }
        Ok(Value::Record(result))
    });

    prim_doc(root, "contains?", "(contains? map :key)", "Return true if the record contains the given key.", |args| {
        if args.len() != 2 {
            return Err("contains? requires exactly 2 arguments: map and key".into());
        }
        let key = keyword_name(&args[1])?;
        match &args[0] {
            Value::Record(r) => Ok(Value::Bool(r.resolve(&[key.as_str()]).is_some())),
            _ => Ok(Value::Bool(false)),
        }
    });

    prim_doc(root, "select-keys", "(select-keys map [:k1 :k2 ...])", "Return a new record containing only the specified keys.", |args| {
        if args.len() != 2 {
            return Err("select-keys requires exactly 2 arguments: map and key-vector".into());
        }
        let keys = match &args[1] {
            Value::List(ks) => ks.clone(),
            _ => return Err("select-keys: second argument must be a vector of keys".into()),
        };
        let mut result = template::DataGraph::new();
        if let Value::Record(r) = &args[0] {
            for k in &keys {
                let key = keyword_name(k)?;
                if let Some(v) = r.resolve(&[key.as_str()]) {
                    result.insert(key, v.clone());
                }
            }
        }
        Ok(Value::Record(result))
    });

    prim_doc(root, "name", "(name :kw) or (name \"str\")", "Return the name portion of a keyword, or the string itself.", |args| {
        if args.len() != 1 {
            return Err("name requires 1 argument".into());
        }
        match &args[0] {
            Value::Keyword { name, .. } => Ok(Value::Text(name.clone())),
            Value::Text(s) => Ok(Value::Text(s.clone())),
            _ => Err("name expects a keyword or string".into()),
        }
    });

    prim_doc(root, "namespace", "(namespace :ns/kw)", "Return the namespace portion of a keyword, or nil if unqualified.", |args| {
        if args.len() != 1 {
            return Err("namespace requires 1 argument".into());
        }
        match &args[0] {
            Value::Keyword { namespace, .. } => match namespace {
                Some(ns) => Ok(Value::Text(ns.clone())),
                None => Ok(Value::Absent),
            },
            _ => Err("namespace expects a keyword".into()),
        }
    });
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a `Value::Fn` backed by the given pure function.
fn prim(name: &str, func: impl Fn(Vec<Value>) -> Result<Value, String> + Send + Sync + 'static) -> Value {
    Value::Fn(Arc::new(PrimitiveFn::new(name, func)))
}

/// Register a primitive with documentation metadata.
fn prim_doc(
    root: &RootEnv,
    name: &str,
    sig: &str,
    doc: &str,
    func: impl Fn(Vec<Value>) -> Result<Value, String> + Send + Sync + 'static,
) {
    let value = prim(name, func);
    root.def_with_doc(
        name,
        value,
        DocEntry {
            name: name.to_string(),
            doc: doc.to_string(),
            arglists: vec![sig.to_string()],
            source: DocSource::Primitive,
        },
    );
}

/// Extract an i64 from a Value.
pub(crate) fn value_to_i64(v: &Value, op: &str) -> Result<i64, String> {
    match v {
        Value::Integer(n) => Ok(*n),
        Value::Text(s) => s.parse::<i64>().map_err(|_| format!("{op}: not a number: {s}")),
        other => Err(format!("{op}: expected a number, got: {other:?}")),
    }
}

/// Extract a key name from a Value (keyword or string).
pub(crate) fn keyword_name(v: &Value) -> Result<String, String> {
    match v {
        Value::Keyword { name, .. } => Ok(name.clone()),
        Value::Text(s) => Ok(s.clone()),
        _ => Err(format!("expected keyword or string, got: {v:?}")),
    }
}

/// Convert a Value to a plain string for comparison / display.
pub(crate) fn value_to_string(v: &Value) -> String {
    match v.display_text() {
        Some(s) => s,
        None => edn::value_to_edn(v),
    }
}
