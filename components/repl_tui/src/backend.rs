use evaluator::{DocEntry, RootEnv};
use std::path::PathBuf;

/// A completion candidate returned by the backend.
pub struct Completion {
    pub candidate: String,
    pub doc: Option<String>,
    pub arglists: Vec<String>,
}

/// Result of evaluating an expression.
pub struct EvalResult {
    pub value: String,
    pub is_error: bool,
}

/// Backend for the REPL — abstracts evaluation and doc lookup.
pub trait ReplBackend: Send + Sync {
    fn eval(&mut self, code: &str) -> EvalResult;
    fn completions(&self, prefix: &str) -> Vec<Completion>;
    fn doc_lookup(&self, symbol: &str) -> Option<String>;
    fn all_symbols(&self) -> Vec<String>;
}

/// Direct backend — in-process evaluator, no external conductor.
///
/// Uses a minimal empty conductor (no site content) so that language
/// primitives and prelude functions work fully, while site-specific
/// functions (`query`, `get-content`, etc.) return informative errors.
pub struct DirectBackend {
    root: RootEnv,
    conductor: conductor::Conductor,
}

impl DirectBackend {
    /// Create a new `DirectBackend` with the prelude loaded.
    ///
    /// Returns an error string if the prelude fails to compile.
    pub fn new() -> Result<Self, String> {
        let repo = site_repository::SiteRepository::builder().build();
        let conductor =
            conductor::Conductor::with_repo(PathBuf::from("/repl-scratch"), repo)
                .map_err(|e| format!("conductor init failed: {e}"))?;

        let root = RootEnv::new();
        evaluator::init_root(&root, &conductor)?;

        Ok(Self { root, conductor })
    }

    fn format_value(v: &template::Value) -> String {
        match v {
            template::Value::Text(s) => format!("{s:?}"),
            template::Value::Integer(n) => n.to_string(),
            template::Value::Bool(b) => b.to_string(),
            template::Value::Absent => "nil".to_string(),
            template::Value::Keyword { namespace: None, name } => format!(":{name}"),
            template::Value::Keyword { namespace: Some(ns), name } => format!(":{ns}/{name}"),
            template::Value::List(items) => {
                let inner: Vec<String> = items.iter().map(Self::format_value).collect();
                format!("({})", inner.join(" "))
            }
            template::Value::Record(g) => {
                let pairs: Vec<String> = g
                    .iter()
                    .map(|(k, v)| format!(":{k} {}", Self::format_value(v)))
                    .collect();
                format!("{{{}}}", pairs.join(", "))
            }
            template::Value::Fn(f) => format!("#<fn {:?}>", f.name()),
            other => format!("{other:?}"),
        }
    }

    fn format_doc_entry(entry: &DocEntry) -> String {
        let mut out = format!("{}\n", entry.name);
        for arglist in &entry.arglists {
            out.push_str(&format!("  {arglist}\n"));
        }
        if !entry.doc.is_empty() {
            out.push_str(&format!("  {}\n", entry.doc));
        }
        out
    }
}

impl ReplBackend for DirectBackend {
    fn eval(&mut self, code: &str) -> EvalResult {
        match evaluator::eval_str_with_root(code, &self.root, &self.conductor) {
            Ok(value) => EvalResult {
                value: Self::format_value(&value),
                is_error: false,
            },
            Err(e) => EvalResult {
                value: e,
                is_error: true,
            },
        }
    }

    fn completions(&self, prefix: &str) -> Vec<Completion> {
        self.root
            .doc_registry
            .completions(prefix)
            .into_iter()
            .map(|e| Completion {
                arglists: e.arglists.clone(),
                doc: Some(e.doc.clone()),
                candidate: e.name,
            })
            .collect()
    }

    fn doc_lookup(&self, symbol: &str) -> Option<String> {
        let entry = self.root.doc_registry.lookup(symbol)?;
        Some(Self::format_doc_entry(&entry))
    }

    fn all_symbols(&self) -> Vec<String> {
        self.root
            .doc_registry
            .all_entries()
            .into_iter()
            .map(|e| e.name)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_backend() -> DirectBackend {
        DirectBackend::new().expect("backend init failed")
    }

    #[test]
    fn eval_arithmetic() {
        let mut b = make_backend();
        let r = b.eval("(+ 1 2)");
        assert!(!r.is_error, "unexpected error: {}", r.value);
        assert_eq!(r.value, "3");
    }

    #[test]
    fn eval_def_persists() {
        let mut b = make_backend();
        b.eval("(def x 42)");
        let r = b.eval("x");
        assert!(!r.is_error, "unexpected error: {}", r.value);
        assert_eq!(r.value, "42");
    }

    #[test]
    fn eval_string_literal() {
        let mut b = make_backend();
        let r = b.eval(r#""hello""#);
        assert!(!r.is_error);
        assert_eq!(r.value, r#""hello""#);
    }

    #[test]
    fn eval_error_on_unknown_symbol() {
        let mut b = make_backend();
        let r = b.eval("no-such-thing");
        assert!(r.is_error, "expected error for unbound symbol");
    }

    #[test]
    fn completions_returns_known_symbols() {
        let b = make_backend();
        // "str" is a registered primitive with docs
        let completions = b.completions("st");
        let names: Vec<&str> = completions.iter().map(|c| c.candidate.as_str()).collect();
        assert!(
            names.contains(&"str"),
            "expected 'str' in completions, got: {names:?}"
        );
    }

    #[test]
    fn doc_lookup_returns_doc_for_plus() {
        let b = make_backend();
        let doc = b.doc_lookup("+");
        assert!(doc.is_some(), "expected doc for '+'");
        let text = doc.unwrap();
        assert!(text.contains('+'), "doc should mention '+': {text}");
    }

    #[test]
    fn all_symbols_is_non_empty() {
        let b = make_backend();
        let syms = b.all_symbols();
        assert!(!syms.is_empty(), "expected at least one symbol in registry");
    }
}
