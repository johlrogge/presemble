use forms::Form;

/// Expand macros in a form. Recursively walks the form tree.
pub fn macroexpand(form: Form) -> Form {
    match form {
        Form::List(ref items) if !items.is_empty() => {
            if let Some(name) = items[0].as_symbol() {
                match name {
                    "->" => expand_thread_first(items[1..].to_vec()),
                    "->>" => expand_thread_last(items[1..].to_vec()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use reader::read;

    #[test]
    fn thread_first_basic() {
        // (-> 1 (+ 2) (* 3)) → (* (+ 1 2) 3)
        let form = read("(-> 1 (+ 2) (* 3))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(* (+ 1 2) 3)");
    }

    #[test]
    fn thread_last_basic() {
        // (->> :post (sort-by :published :desc) (take 3))
        // → (take (sort-by :post :published :desc) 3)
        let form = read("(->> :post (sort-by :published :desc) (take 3))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(
            expanded.to_string(),
            "(take (sort-by :post :published :desc) 3)"
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
        // (-> x f) → (f x)
        let form = read("(-> 1 +)").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(+ 1)");
    }

    #[test]
    fn thread_last_bare_symbol() {
        // (->> x f) → (f x)
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
        // (-> (-> 1 (+ 2)) (* 3)) → (* (+ 1 2) 3)
        let form = read("(-> (-> 1 (+ 2)) (* 3))").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "(* (+ 1 2) 3)");
    }

    #[test]
    fn vector_children_expand_recursively() {
        // [(-> 1 (+ 2))] → [(+ 1 2)]
        let form = read("[(-> 1 (+ 2))]").unwrap();
        let expanded = macroexpand(form);
        assert_eq!(expanded.to_string(), "[(+ 1 2)]");
    }
}
