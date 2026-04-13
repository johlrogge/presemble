use forms::Form;

/// Counter for generating unique gensym names.
/// Using a thread-local to avoid global state issues in tests.
use std::cell::Cell;
thread_local! {
    static GENSYM_COUNTER: Cell<u64> = const { Cell::new(0) };
}

fn gensym(prefix: &str) -> String {
    GENSYM_COUNTER.with(|c| {
        let n = c.get();
        c.set(n + 1);
        format!("{}__{}", prefix, n)
    })
}

/// Expand macros in a form. Recursively walks the form tree.
pub fn macroexpand(form: Form) -> Form {
    match form {
        Form::List(ref items) if !items.is_empty() => {
            if let Some(name) = items[0].as_symbol() {
                match name {
                    "->" => expand_thread_first(items[1..].to_vec()),
                    "->>" => expand_thread_last(items[1..].to_vec()),
                    "as->" => expand_as_thread(items[1..].to_vec()),
                    "some->" => expand_some_thread(items[1..].to_vec(), false),
                    "some->>" => expand_some_thread(items[1..].to_vec(), true),
                    "cond->" => expand_cond_thread(items[1..].to_vec(), false),
                    "cond->>" => expand_cond_thread(items[1..].to_vec(), true),
                    "when" => expand_when(items[1..].to_vec()),
                    "when-not" => expand_when_not(items[1..].to_vec()),
                    "if-not" => expand_if_not(items[1..].to_vec()),
                    "if-let" => expand_if_let(items[1..].to_vec()),
                    "when-let" => expand_when_let(items[1..].to_vec()),
                    "cond" => expand_cond(items[1..].to_vec()),
                    "and" => expand_and(items[1..].to_vec()),
                    "or" => expand_or(items[1..].to_vec()),
                    "defn" => expand_defn(items[1..].to_vec()),
                    _ => {
                        // Not a macro — recursively expand children
                        Form::List(items.iter().map(|f| macroexpand(f.clone())).collect())
                    }
                }
            } else {
                Form::List(items.iter().map(|f| macroexpand(f.clone())).collect())
            }
        }
        Form::List(items) => Form::List(items.into_iter().map(macroexpand).collect()),
        Form::Vector(items) => Form::Vector(items.into_iter().map(macroexpand).collect()),
        // Other forms pass through unchanged
        other => other,
    }
}

/// Thread-first macro: `(-> x (f a) (g b))` → `(g (f x a) b)`
fn expand_thread_first(mut args: Vec<Form>) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }
    let mut result = macroexpand(args.remove(0));
    for form in args {
        result = match macroexpand(form) {
            Form::List(mut items) => {
                // Insert result as first argument: (f a b) → (f result a b)
                items.insert(1.min(items.len()), result);
                Form::List(items)
            }
            Form::Symbol(s) => {
                // Bare symbol: (-> x f) → (f x)
                Form::List(vec![Form::Symbol(s), result])
            }
            other => {
                // Treat as function call
                Form::List(vec![other, result])
            }
        };
    }
    result
}

/// Thread-last macro: `(->> x (f a) (g b))` → `(g b (f a x))`
fn expand_thread_last(mut args: Vec<Form>) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }
    let mut result = macroexpand(args.remove(0));
    for form in args {
        result = match macroexpand(form) {
            Form::List(mut items) => {
                // Append result as last argument: (f a b) → (f a b result)
                items.push(result);
                Form::List(items)
            }
            Form::Symbol(s) => {
                // Bare symbol: (->> x f) → (f x)
                Form::List(vec![Form::Symbol(s), result])
            }
            other => {
                Form::List(vec![other, result])
            }
        };
    }
    result
}

/// `(as-> expr name form1 form2 ...)` → `(let [name expr name form1 name form2 ...] name)`
fn expand_as_thread(args: Vec<Form>) -> Form {
    if args.len() < 2 {
        return Form::Nil;
    }
    let expr = macroexpand(args[0].clone());
    let name = args[1].clone();
    let forms = &args[2..];

    // Build let bindings: [name expr name form1 name form2 ...]
    let mut bindings = vec![name.clone(), expr];
    for form in forms {
        bindings.push(name.clone());
        bindings.push(macroexpand(form.clone()));
    }

    macroexpand(Form::List(vec![
        Form::Symbol("let".into()),
        Form::Vector(bindings),
        name,
    ]))
}

/// Helper to thread a value into a form at first or last position.
fn thread_into(value: Form, form: Form, last: bool) -> Form {
    match form {
        Form::List(mut items) => {
            if last {
                items.push(value);
            } else {
                items.insert(1.min(items.len()), value);
            }
            Form::List(items)
        }
        Form::Symbol(s) => Form::List(vec![Form::Symbol(s), value]),
        other => Form::List(vec![other, value]),
    }
}

/// `(some-> x (f a) (g b))` — thread-first, short-circuit on nil.
/// `(some->> x (f a) (g b))` — thread-last, short-circuit on nil.
///
/// Expands to nested let+if:
/// `(let [g__0 x] (if g__0 (let [g__1 (f g__0 a)] (if g__1 (g g__1 b) nil)) nil))`
fn expand_some_thread(args: Vec<Form>, last: bool) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }

    let expr = macroexpand(args[0].clone());
    let forms = &args[1..];

    if forms.is_empty() {
        return expr;
    }

    // Build inside-out: start from the last step and work backwards
    let sym0 = gensym("some");
    let mut sym_names: Vec<String> = vec![sym0];

    for _ in forms {
        sym_names.push(gensym("some"));
    }

    // The innermost value is the last symbol
    let last_sym = Form::Symbol(sym_names.last().unwrap().clone());

    // Build from outside-in: expr → sym_names[0], then each step
    // We build a chain: let [s0 expr] (if s0 (let [s1 (f s0 ...)] (if s1 ... s1_result)) nil)
    let mut result = last_sym;

    for i in (0..forms.len()).rev() {
        let step_sym = Form::Symbol(sym_names[i + 1].clone());
        let prev_sym = Form::Symbol(sym_names[i].clone());
        let form = macroexpand(forms[i].clone());
        let call = thread_into(prev_sym.clone(), form, last);
        // (let [step_sym call] (if step_sym result nil))
        result = Form::List(vec![
            Form::Symbol("let".into()),
            Form::Vector(vec![step_sym.clone(), call]),
            Form::List(vec![
                Form::Symbol("if".into()),
                step_sym,
                result,
                Form::Nil,
            ]),
        ]);
    }

    // Wrap with the initial binding
    macroexpand(Form::List(vec![
        Form::Symbol("let".into()),
        Form::Vector(vec![Form::Symbol(sym_names[0].clone()), expr]),
        Form::List(vec![
            Form::Symbol("if".into()),
            Form::Symbol(sym_names[0].clone()),
            result,
            Form::Nil,
        ]),
    ]))
}

/// `(cond-> x test1 (f a) test2 (g b))` — thread-first conditionally.
/// `(cond->> x test1 (f a) test2 (g b))` — thread-last conditionally.
///
/// Does NOT short-circuit. Expands to nested let:
/// `(let [g__0 x g__1 (if test1 (f g__0 a) g__0) g__2 (if test2 (g g__1 b) g__1)] g__2)`
fn expand_cond_thread(args: Vec<Form>, last: bool) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }

    let expr = macroexpand(args[0].clone());
    let steps = &args[1..];

    if !steps.len().is_multiple_of(2) {
        // Odd number of test/form pairs — return Nil (error-like behavior)
        return Form::Nil;
    }

    if steps.is_empty() {
        return expr;
    }

    let sym0 = gensym("cond");
    let mut bindings = vec![Form::Symbol(sym0.clone()), expr];
    let mut prev_sym = sym0;

    let pairs: Vec<_> = steps.chunks(2).collect();
    for pair in &pairs {
        let test = macroexpand(pair[0].clone());
        let form = macroexpand(pair[1].clone());
        let new_sym = gensym("cond");
        let prev = Form::Symbol(prev_sym.clone());
        let call = thread_into(prev.clone(), form, last);
        // (if test call prev)
        let branch = Form::List(vec![
            Form::Symbol("if".into()),
            test,
            call,
            prev,
        ]);
        bindings.push(Form::Symbol(new_sym.clone()));
        bindings.push(branch);
        prev_sym = new_sym;
    }

    macroexpand(Form::List(vec![
        Form::Symbol("let".into()),
        Form::Vector(bindings),
        Form::Symbol(prev_sym),
    ]))
}

/// `(when test body1 body2)` → `(if test (do body1 body2) nil)`
fn expand_when(args: Vec<Form>) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }
    let test = macroexpand(args[0].clone());
    let body = args[1..].iter().map(|f| macroexpand(f.clone())).collect::<Vec<_>>();
    let do_form = if body.len() == 1 {
        body.into_iter().next().unwrap()
    } else {
        let mut do_items = vec![Form::Symbol("do".into())];
        do_items.extend(body);
        Form::List(do_items)
    };
    Form::List(vec![
        Form::Symbol("if".into()),
        test,
        do_form,
        Form::Nil,
    ])
}

/// `(when-not test body...)` → `(if test nil (do body...))`
fn expand_when_not(args: Vec<Form>) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }
    let test = macroexpand(args[0].clone());
    let body = args[1..].iter().map(|f| macroexpand(f.clone())).collect::<Vec<_>>();
    let do_form = if body.len() == 1 {
        body.into_iter().next().unwrap()
    } else {
        let mut do_items = vec![Form::Symbol("do".into())];
        do_items.extend(body);
        Form::List(do_items)
    };
    Form::List(vec![
        Form::Symbol("if".into()),
        test,
        Form::Nil,
        do_form,
    ])
}

/// `(if-not test then else)` → `(if test else then)`
fn expand_if_not(args: Vec<Form>) -> Form {
    if args.len() < 2 {
        return Form::Nil;
    }
    let test = macroexpand(args[0].clone());
    let then = macroexpand(args[1].clone());
    let else_form = if args.len() > 2 {
        macroexpand(args[2].clone())
    } else {
        Form::Nil
    };
    Form::List(vec![
        Form::Symbol("if".into()),
        test,
        else_form,
        then,
    ])
}

/// `(if-let [x expr] then else)` → `(let [x expr] (if x then else))`
fn expand_if_let(args: Vec<Form>) -> Form {
    if args.len() < 2 {
        return Form::Nil;
    }
    let binding = match &args[0] {
        Form::Vector(items) if items.len() >= 2 => items.clone(),
        _ => return Form::Nil,
    };
    let bind_sym = macroexpand(binding[0].clone());
    let bind_expr = macroexpand(binding[1].clone());
    let then = macroexpand(args[1].clone());
    let else_form = if args.len() > 2 {
        macroexpand(args[2].clone())
    } else {
        Form::Nil
    };
    Form::List(vec![
        Form::Symbol("let".into()),
        Form::Vector(vec![bind_sym.clone(), bind_expr]),
        Form::List(vec![
            Form::Symbol("if".into()),
            bind_sym,
            then,
            else_form,
        ]),
    ])
}

/// `(when-let [x expr] body...)` → `(let [x expr] (if x (do body...) nil))`
fn expand_when_let(args: Vec<Form>) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }
    let binding = match &args[0] {
        Form::Vector(items) if items.len() >= 2 => items.clone(),
        _ => return Form::Nil,
    };
    let bind_sym = macroexpand(binding[0].clone());
    let bind_expr = macroexpand(binding[1].clone());
    let body = args[1..].iter().map(|f| macroexpand(f.clone())).collect::<Vec<_>>();
    let do_form = if body.len() == 1 {
        body.into_iter().next().unwrap()
    } else {
        let mut do_items = vec![Form::Symbol("do".into())];
        do_items.extend(body);
        Form::List(do_items)
    };
    Form::List(vec![
        Form::Symbol("let".into()),
        Form::Vector(vec![bind_sym.clone(), bind_expr]),
        Form::List(vec![
            Form::Symbol("if".into()),
            bind_sym,
            do_form,
            Form::Nil,
        ]),
    ])
}

/// `(cond test1 expr1 test2 expr2 :else expr3)` → `(if test1 expr1 (if test2 expr2 expr3))`
/// Pairs of test/expr. `:else` keyword is treated as always-true.
fn expand_cond(args: Vec<Form>) -> Form {
    if args.is_empty() {
        return Form::Nil;
    }
    if !args.len().is_multiple_of(2) {
        // Odd — treat last element as a bare else expression (unusual but defensive)
        // Actually, odd-count cond is an error in Clojure. Return Nil.
        return Form::Nil;
    }

    // Build from right to left
    let pairs: Vec<_> = args.chunks(2).collect();
    let mut result = Form::Nil;

    for pair in pairs.iter().rev() {
        let test = macroexpand(pair[0].clone());
        let expr = macroexpand(pair[1].clone());

        // `:else` keyword is treated as true
        let is_else = matches!(&test, Form::Keyword { namespace: None, name } if name == "else");

        if is_else {
            // This branch always taken — use expr directly as the else of what comes before
            result = expr;
        } else {
            result = Form::List(vec![
                Form::Symbol("if".into()),
                test,
                expr,
                result,
            ]);
        }
    }

    result
}

/// `(and)` → `true`
/// `(and x)` → `x`
/// `(and x y)` → `(let [G x] (if G y G))` where G is a gensym
/// `(and x y z)` → nested let/if chain, short-circuiting on falsy
fn expand_and(args: Vec<Form>) -> Form {
    match args.len() {
        0 => Form::Bool(true),
        1 => macroexpand(args.into_iter().next().unwrap()),
        _ => {
            let head = macroexpand(args[0].clone());
            let rest = args[1..].to_vec();
            let sym = Form::Symbol(gensym("and"));
            let rest_expanded = if rest.len() == 1 {
                macroexpand(rest.into_iter().next().unwrap())
            } else {
                // Recursive: (and rest...)
                expand_and(rest)
            };
            Form::List(vec![
                Form::Symbol("let".into()),
                Form::Vector(vec![sym.clone(), head]),
                Form::List(vec![
                    Form::Symbol("if".into()),
                    sym.clone(),
                    rest_expanded,
                    sym,
                ]),
            ])
        }
    }
}

/// `(or)` → `nil`
/// `(or x)` → `x`
/// `(or x y)` → `(let [G x] (if G G y))` where G is a gensym
/// `(or x y z)` → nested let/if chain, short-circuiting on truthy
fn expand_or(args: Vec<Form>) -> Form {
    match args.len() {
        0 => Form::Nil,
        1 => macroexpand(args.into_iter().next().unwrap()),
        _ => {
            let head = macroexpand(args[0].clone());
            let rest = args[1..].to_vec();
            let sym = Form::Symbol(gensym("or"));
            let rest_expanded = if rest.len() == 1 {
                macroexpand(rest.into_iter().next().unwrap())
            } else {
                // Recursive: (or rest...)
                expand_or(rest)
            };
            Form::List(vec![
                Form::Symbol("let".into()),
                Form::Vector(vec![sym.clone(), head]),
                Form::List(vec![
                    Form::Symbol("if".into()),
                    sym.clone(),
                    sym,
                    rest_expanded,
                ]),
            ])
        }
    }
}

/// `(defn name [args] body...)` → `(def name (fn name [args] body...))`
/// `(defn name "docstring" [args] body...)` →
///   `(do (def name (fn name [args] body...)) (def-doc! name "docstring" "[args]"))`
fn expand_defn(args: Vec<Form>) -> Form {
    if args.len() < 2 {
        return Form::Nil;
    }
    let name = args[0].clone();

    // Check if second element is a docstring (string) or params (vector)
    let (has_doc, doc_str, params_idx) = if let Form::Str(doc) = &args[1] {
        if args.len() < 3 {
            return Form::Nil;
        }
        (true, Some(doc.clone()), 2usize)
    } else {
        (false, None, 1usize)
    };

    let body_forms = &args[params_idx..];
    let params = body_forms[0].clone();
    let body: Vec<Form> = body_forms[1..].iter().map(|f| macroexpand(f.clone())).collect();

    // Build (fn name [args] body...) — for single arity
    // or (fn name ([args1] body1) ([args2] body2) ...) for multi-arity
    let fn_form = match &params {
        Form::Vector(_) => {
            // Single-arity
            let mut parts = vec![
                Form::Symbol("fn".into()),
                name.clone(),
                params,
            ];
            parts.extend(body);
            Form::List(parts)
        }
        Form::List(_) => {
            // Multi-arity: params is actually the first clause, body_forms has all clauses
            let mut parts = vec![
                Form::Symbol("fn".into()),
                name.clone(),
            ];
            for clause in body_forms {
                parts.push(macroexpand(clause.clone()));
            }
            Form::List(parts)
        }
        _ => {
            // Malformed — pass through
            let mut parts = vec![
                Form::Symbol("fn".into()),
                name.clone(),
                params,
            ];
            parts.extend(body);
            Form::List(parts)
        }
    };

    // Build (def name (fn name ...))
    let def_form = Form::List(vec![
        Form::Symbol("def".into()),
        name.clone(),
        fn_form,
    ]);

    if has_doc {
        let doc = doc_str.unwrap();
        // Extract arglist strings
        let arglists = extract_arglists(body_forms);

        // Build (def-doc! name "doc" "[args]" ...)
        let mut doc_form_items = vec![
            Form::Symbol("def-doc!".into()),
            name,
            Form::Str(doc),
        ];
        for arglist in arglists {
            doc_form_items.push(Form::Str(arglist));
        }

        // Expand as (do def_form doc_form)
        macroexpand(Form::List(vec![
            Form::Symbol("do".into()),
            def_form,
            Form::List(doc_form_items),
        ]))
    } else {
        macroexpand(def_form)
    }
}

/// Extract arglist strings from defn body forms (everything after the optional docstring).
/// For single-arity `(defn name "doc" [x y] body)`: body_forms = `[[x y] body]` → `["[x y]"]`
/// For multi-arity `(defn name "doc" ([x] body1) ([x y] body2))`:
///   body_forms = `[([x] body1) ([x y] body2)]` → `["[x]", "[x y]"]`
fn extract_arglists(body_forms: &[Form]) -> Vec<String> {
    if body_forms.is_empty() {
        return vec![];
    }
    match &body_forms[0] {
        Form::Vector(_) => {
            // Single arity: first form is the param vector
            vec![body_forms[0].to_string()]
        }
        Form::List(items) if !items.is_empty() && matches!(&items[0], Form::Vector(_)) => {
            // Multi-arity: each form in body_forms is a clause starting with a param vector
            body_forms
                .iter()
                .filter_map(|f| {
                    if let Form::List(clause_items) = f {
                        clause_items.first().map(|v| v.to_string())
                    } else {
                        None
                    }
                })
                .collect()
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reader::read;

    // ── existing tests ──────────────────────────────────────────────────────

    #[test]
    fn thread_first_basic() {
        // (-> 1 (+ 2) (* 3)) → (* (+ 1 2) 3)
        let form = read("(-> 1 (+ 2) (* 3))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(* (+ 1 2) 3)");
    }

    #[test]
    fn thread_last_basic() {
        let form = read("(->> :post (sort-by :published :desc) (take 3))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(take 3 (sort-by :published :desc :post))"
        );
    }

    #[test]
    fn non_macro_list_passes_through() {
        let form = read("(+ 1 2)").unwrap();
        let expanded = macroexpand(form.clone());
        assert_eq!(expanded, form);
    }

    #[test]
    fn non_list_form_passes_through() {
        let form = read(":keyword").unwrap();
        let expanded = macroexpand(form.clone());
        assert_eq!(expanded, form);

        let form2 = read("42").unwrap();
        let expanded2 = macroexpand(form2.clone());
        assert_eq!(expanded2, form2);
    }

    #[test]
    fn thread_first_bare_symbol() {
        let form = read("(-> 1 +)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(+ 1)");
    }

    #[test]
    fn thread_last_bare_symbol() {
        let form = read("(->> 1 +)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(+ 1)");
    }

    #[test]
    fn thread_first_empty_returns_nil() {
        let form = read("(->)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Nil);
    }

    #[test]
    fn thread_last_empty_returns_nil() {
        let form = read("(->>)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Nil);
    }

    #[test]
    fn nested_macros_expand_recursively() {
        let form = read("(-> (-> 1 (+ 2)) (* 3))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(* (+ 1 2) 3)");
    }

    #[test]
    fn vector_children_expand_recursively() {
        let form = read("[(-> 1 (+ 2))]").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "[(+ 1 2)]");
    }

    // ── as-> ────────────────────────────────────────────────────────────────

    #[test]
    fn as_thread_basic() {
        // (as-> 0 x (+ x 1) (* x 2)) → (let [x 0 x (+ x 1) x (* x 2)] x)
        let form = read("(as-> 0 x (+ x 1) (* x 2))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(let [x 0 x (+ x 1) x (* x 2)] x)");
    }

    #[test]
    fn as_thread_single_form() {
        // (as-> 0 x (inc x)) → (let [x 0 x (inc x)] x)
        let form = read("(as-> 0 x (inc x))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(let [x 0 x (inc x)] x)");
    }

    #[test]
    fn as_thread_no_forms() {
        // (as-> expr name) — no transformation forms → (let [name expr] name)
        let form = read("(as-> 42 x)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(let [x 42] x)");
    }

    // ── when ────────────────────────────────────────────────────────────────

    #[test]
    fn when_single_body() {
        // (when true 42) → (if true 42 nil)
        let form = read("(when true 42)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if true 42 nil)");
    }

    #[test]
    fn when_multiple_body() {
        // (when true (foo) (bar)) → (if true (do (foo) (bar)) nil)
        let form = read("(when true (foo) (bar))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if true (do (foo) (bar)) nil)");
    }

    #[test]
    fn when_empty_returns_nil() {
        let form = read("(when)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Nil);
    }

    // ── when-not ────────────────────────────────────────────────────────────

    #[test]
    fn when_not_single_body() {
        // (when-not false 42) → (if false nil 42)
        let form = read("(when-not false 42)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if false nil 42)");
    }

    #[test]
    fn when_not_multiple_body() {
        // (when-not false (foo) (bar)) → (if false nil (do (foo) (bar)))
        let form = read("(when-not false (foo) (bar))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if false nil (do (foo) (bar)))");
    }

    // ── if-not ──────────────────────────────────────────────────────────────

    #[test]
    fn if_not_with_else() {
        // (if-not test then else) → (if test else then)
        let form = read("(if-not (nil? x) :yes :no)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if (nil? x) :no :yes)");
    }

    #[test]
    fn if_not_without_else() {
        // (if-not test then) → (if test nil then)
        let form = read("(if-not false :yes)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if false nil :yes)");
    }

    // ── if-let ──────────────────────────────────────────────────────────────

    #[test]
    fn if_let_with_else() {
        // (if-let [x (get m :k)] (use x) :missing) → (let [x (get m :k)] (if x (use x) :missing))
        let form = read("(if-let [x (get m :k)] (use x) :missing)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(let [x (get m :k)] (if x (use x) :missing))"
        );
    }

    #[test]
    fn if_let_without_else() {
        let form = read("(if-let [x (f)] (do-thing x))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(let [x (f)] (if x (do-thing x) nil))");
    }

    // ── when-let ────────────────────────────────────────────────────────────

    #[test]
    fn when_let_single_body() {
        // (when-let [x (f)] (use x)) → (let [x (f)] (if x (use x) nil))
        let form = read("(when-let [x (f)] (use x))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(let [x (f)] (if x (use x) nil))");
    }

    #[test]
    fn when_let_multiple_body() {
        let form = read("(when-let [x (f)] (a x) (b x))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(let [x (f)] (if x (do (a x) (b x)) nil))"
        );
    }

    // ── cond ────────────────────────────────────────────────────────────────

    #[test]
    fn cond_basic() {
        // (cond (= x 1) :one (= x 2) :two :else :other)
        // → (if (= x 1) :one (if (= x 2) :two :other))
        let form = read("(cond (= x 1) :one (= x 2) :two :else :other)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(if (= x 1) :one (if (= x 2) :two :other))"
        );
    }

    #[test]
    fn cond_no_else() {
        // (cond (= x 1) :one (= x 2) :two)
        // → (if (= x 1) :one (if (= x 2) :two nil))
        let form = read("(cond (= x 1) :one (= x 2) :two)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(if (= x 1) :one (if (= x 2) :two nil))"
        );
    }

    #[test]
    fn cond_empty_returns_nil() {
        let form = read("(cond)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Nil);
    }

    #[test]
    fn cond_single_pair() {
        let form = read("(cond true 42)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(if true 42 nil)");
    }

    // ── and ─────────────────────────────────────────────────────────────────

    #[test]
    fn and_zero_args() {
        let form = read("(and)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Bool(true));
    }

    #[test]
    fn and_one_arg() {
        let form = read("(and x)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "x");
    }

    #[test]
    fn and_two_args() {
        // (and x y) → (let [and__N x] (if and__N y and__N))
        let form = read("(and x y)").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(let [and__"), "expected let with gensym, got: {s}");
        assert!(s.contains("(if and__"), "expected if with gensym, got: {s}");
        assert!(s.contains(" y "), "expected y in then branch, got: {s}");
    }

    #[test]
    fn and_three_args() {
        // (and x y z) → nested let/if with gensym names
        let form = read("(and x y z)").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(let [and__"), "expected outer let, got: {s}");
        // Should have two nested let bindings (one per and level)
        assert_eq!(s.matches("(let [and__").count(), 2, "expected 2 nested lets, got: {s}");
        assert!(s.contains(" z "), "expected z in innermost branch, got: {s}");
    }

    // ── or ──────────────────────────────────────────────────────────────────

    #[test]
    fn or_zero_args() {
        let form = read("(or)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Nil);
    }

    #[test]
    fn or_one_arg() {
        let form = read("(or x)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "x");
    }

    #[test]
    fn or_two_args() {
        // (or x y) → (let [or__N x] (if or__N or__N y))
        let form = read("(or x y)").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(let [or__"), "expected let with gensym, got: {s}");
        assert!(s.contains("(if or__"), "expected if with gensym, got: {s}");
        assert!(s.contains(" y)"), "expected y as else branch, got: {s}");
    }

    #[test]
    fn or_three_args() {
        // (or x y z) → nested let/if with gensym names
        let form = read("(or x y z)").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(let [or__"), "expected outer let, got: {s}");
        assert_eq!(s.matches("(let [or__").count(), 2, "expected 2 nested lets, got: {s}");
        assert!(s.contains(" z)"), "expected z in innermost else, got: {s}");
    }

    // ── defn ────────────────────────────────────────────────────────────────

    #[test]
    fn defn_basic() {
        // (defn add [x y] (+ x y)) → (def add (fn add [x y] (+ x y)))
        let form = read("(defn add [x y] (+ x y))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(def add (fn add [x y] (+ x y)))");
    }

    #[test]
    fn defn_with_docstring() {
        // (defn add "adds two numbers" [x y] (+ x y)) →
        // (do (def add (fn add [x y] (+ x y))) (def-doc! add "adds two numbers" "[x y]"))
        let form = read(r#"(defn add "adds two numbers" [x y] (+ x y))"#).unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            r#"(do (def add (fn add [x y] (+ x y))) (def-doc! add "adds two numbers" "[x y]"))"#
        );
    }

    #[test]
    fn defn_multi_body() {
        // (defn greet [name] (str "Hello, " name) (println "done"))
        let form = read(r#"(defn greet [name] (str "Hello, " name) (println "done"))"#).unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            r#"(def greet (fn greet [name] (str "Hello, " name) (println "done")))"#
        );
    }

    // ── some-> ──────────────────────────────────────────────────────────────

    #[test]
    fn some_thread_first_nil_short_circuit() {
        // (some-> nil (+ 1)) — result should be nil-guarded
        // We can't evaluate here, but we verify the structure is if-guarded
        let form = read("(some-> x (f a))").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        // Should contain "if" and "nil" for short-circuit
        assert!(s.contains("if"), "expected 'if' in: {s}");
        assert!(s.contains("nil"), "expected 'nil' in: {s}");
        assert!(s.contains("(f "), "expected call '(f ' in: {s}");
    }

    #[test]
    fn some_thread_first_no_forms() {
        // (some-> x) → x
        let form = read("(some-> x)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "x");
    }

    #[test]
    fn some_thread_last_structure() {
        // (some->> x (f a)) — value threaded at last position
        let form = read("(some->> x (f a))").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.contains("(f a "), "expected thread-last position in: {s}");
    }

    // ── cond-> ──────────────────────────────────────────────────────────────

    #[test]
    fn cond_thread_first_basic() {
        // (cond-> x true (f a) false (g b))
        // → (let [s0 x s1 (if true (f s0 a) s0) s2 (if false (g s1 b) s1)] s2)
        let form = read("(cond-> x true (f a) false (g b))").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        // Verify structure: let with conditional branches
        assert!(s.starts_with("(let ["), "expected let form, got: {s}");
        assert!(s.contains("(if true (f "), "expected true branch with thread-first: {s}");
        assert!(s.contains("(if false (g "), "expected false branch with thread-first: {s}");
    }

    #[test]
    fn cond_thread_last_basic() {
        // (cond->> x true (f a) false (g b)) — value threaded at last position
        let form = read("(cond->> x true (f a) false (g b))").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(let ["), "expected let form, got: {s}");
        // In thread-last, value comes after args: (f a value)
        assert!(s.contains("(if true (f a "), "expected thread-last position: {s}");
    }

    #[test]
    fn cond_thread_empty_returns_nil() {
        let form = read("(cond->)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded, Form::Nil);
    }

    // ── nested macro expansion ───────────────────────────────────────────────

    #[test]
    fn when_inside_defn() {
        // (defn f [x] (when x (+ x 1))) → (def f (fn f [x] (if x (+ x 1) nil)))
        let form = read("(defn f [x] (when x (+ x 1)))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(def f (fn f [x] (if x (+ x 1) nil)))"
        );
    }

    #[test]
    fn cond_inside_defn() {
        let form = read("(defn check [x] (cond (= x 1) :one :else :other))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(def check (fn check [x] (if (= x 1) :one :other)))"
        );
    }

    #[test]
    fn and_inside_when() {
        // (when (and a b) body) → (if (let [and__N a] (if and__N b and__N)) body nil)
        let form = read("(when (and a b) body)").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(if (let [and__"), "expected if wrapping and-let, got: {s}");
        assert!(s.ends_with(" body nil)"), "expected body and nil, got: {s}");
    }

    #[test]
    fn or_inside_cond() {
        // (cond (or a b) :yes :else :no)
        let form = read("(cond (or a b) :yes :else :no)").unwrap();
        let expanded = macroexpand(form);
        let s = expanded.to_string();
        assert!(s.starts_with("(if (let [or__"), "expected if wrapping or-let, got: {s}");
        assert!(s.contains(":yes"), "expected :yes branch, got: {s}");
        assert!(s.contains(":no"), "expected :no branch, got: {s}");
    }
}
