use std::sync::{Arc, RwLock};
use template::Value;

/// Lexical environment — immutable scope with parent chain.
/// Each `let` / `fn` body creates a new child scope via `with_parent`.
/// All child scopes are immutable; only the root env is mutable (via `RootEnv`).
#[derive(Debug, Clone)]
pub struct Env {
    pub(crate) bindings: im::HashMap<String, Value>,
    parent: Option<Arc<Env>>,
}

impl Default for Env {
    fn default() -> Self {
        Env::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Env {
            bindings: im::HashMap::new(),
            parent: None,
        }
    }

    pub fn with_parent(parent: Arc<Env>) -> Self {
        Env {
            bindings: im::HashMap::new(),
            parent: Some(parent),
        }
    }

    /// Look up a symbol, walking the parent chain.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.bindings
            .get(name)
            .cloned()
            .or_else(|| self.parent.as_ref().and_then(|p| p.get(name)))
    }

    /// Return a new `Env` with the binding added (immutable update).
    pub fn set(&self, name: impl Into<String>, value: Value) -> Env {
        Env {
            bindings: self.bindings.update(name.into(), value),
            parent: self.parent.clone(),
        }
    }
}

/// Root environment — mutable top-level namespace for `def`.
/// Shared across evaluations in the same session.
#[derive(Debug, Clone)]
pub struct RootEnv {
    inner: Arc<RwLock<Env>>,
}

impl Default for RootEnv {
    fn default() -> Self {
        RootEnv::new()
    }
}

impl RootEnv {
    pub fn new() -> Self {
        RootEnv {
            inner: Arc::new(RwLock::new(Env::new())),
        }
    }

    /// Define a binding at the top level (used by `def`).
    pub fn def(&self, name: impl Into<String>, value: Value) {
        let mut env = self.inner.write().unwrap();
        env.bindings = env.bindings.update(name.into(), value);
    }

    /// Get a snapshot of the current root as an immutable `Arc<Env>`.
    /// Child scopes can use this as their parent.
    pub fn snapshot(&self) -> Arc<Env> {
        Arc::new(self.inner.read().unwrap().clone())
    }

    /// Look up directly in the root.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.inner.read().unwrap().get(name)
    }
}
