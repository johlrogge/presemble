use std::any::Any;
use std::sync::Arc;
use forms::Form;
use template::{Callable, Value};
use crate::env::{Env, RootEnv};

// ---------------------------------------------------------------------------
// FnArity
// ---------------------------------------------------------------------------

/// A single arity of a `fn` form.
#[derive(Debug, Clone)]
pub struct FnArity {
    /// Fixed positional parameters.
    pub params: Vec<String>,
    /// Optional rest parameter (after `&`).
    pub rest_param: Option<String>,
    /// Body forms — evaluated in sequence; last value is the result.
    pub body: Vec<Form>,
}

impl FnArity {
    /// Returns true if `argc` arguments match this arity.
    pub fn matches(&self, argc: usize) -> bool {
        if self.rest_param.is_some() {
            argc >= self.params.len()
        } else {
            argc == self.params.len()
        }
    }
}

// ---------------------------------------------------------------------------
// Closure
// ---------------------------------------------------------------------------

/// A user-defined function: captures its defining lexical environment.
#[derive(Debug, Clone)]
pub struct Closure {
    pub name: Option<String>,
    pub arities: Vec<FnArity>,
    /// The lexical environment captured at closure creation time.
    pub env: Arc<Env>,
    /// Root env reference — needed for `def`d names and primitives.
    pub root: RootEnv,
}

impl Closure {
    /// Find the arity that matches `argc` arguments.
    pub fn match_arity(&self, argc: usize) -> Result<&FnArity, String> {
        self.arities
            .iter()
            .find(|a| a.matches(argc))
            .ok_or_else(|| {
                let sigs: Vec<String> = self
                    .arities
                    .iter()
                    .map(|a| {
                        if a.rest_param.is_some() {
                            format!("{}+", a.params.len())
                        } else {
                            a.params.len().to_string()
                        }
                    })
                    .collect();
                format!(
                    "wrong number of arguments: {} (expected {})",
                    argc,
                    sigs.join(" or ")
                )
            })
    }

    /// Apply the closure with the given arguments and conductor reference.
    /// This is distinct from `Callable::call` because it needs the conductor
    /// for conductor-backed primitives invoked from within the body.
    pub fn apply(
        &self,
        args: Vec<Value>,
        conductor: &conductor::Conductor,
    ) -> Result<Value, String> {
        let arity = self.match_arity(args.len())?;

        // Build a new local env with the captured env as parent.
        let mut local = Env::with_parent(self.env.clone());

        // Bind positional parameters.
        for (param, arg) in arity.params.iter().zip(args.iter()) {
            local = local.set(param.as_str(), arg.clone());
        }

        // Bind rest parameter (as a List).
        if let Some(rest) = &arity.rest_param {
            let rest_args = args[arity.params.len()..].to_vec();
            local = local.set(rest.as_str(), Value::List(rest_args));
        }

        let local = Arc::new(local);

        // Evaluate body forms; return the last.
        let mut result = Value::Absent;
        for form in &arity.body {
            result = crate::eval_in_env(form, &local, &self.root, conductor)?;
        }
        Ok(result)
    }
}

impl Callable for Closure {
    fn call(&self, _args: Vec<Value>) -> Result<Value, String> {
        // Closures require a conductor for evaluation.
        // Use `Closure::apply` (dispatched by the evaluator via downcast) instead.
        Err(format!(
            "closure '{}' cannot be called without conductor context",
            self.name.as_deref().unwrap_or("anonymous")
        ))
    }

    fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ---------------------------------------------------------------------------
// PrimitiveFn
// ---------------------------------------------------------------------------

/// A Rust-backed primitive function that does not need a conductor reference.
#[derive(Clone)]
pub struct PrimitiveFn {
    pub pname: String,
    pub func: Arc<dyn Fn(Vec<Value>) -> Result<Value, String> + Send + Sync>,
}

impl std::fmt::Debug for PrimitiveFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PrimitiveFn({})", self.pname)
    }
}

impl Callable for PrimitiveFn {
    fn call(&self, args: Vec<Value>) -> Result<Value, String> {
        (self.func)(args)
    }

    fn name(&self) -> Option<&str> {
        Some(&self.pname)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl PrimitiveFn {
    pub fn new(
        name: impl Into<String>,
        func: impl Fn(Vec<Value>) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        PrimitiveFn {
            pname: name.into(),
            func: Arc::new(func),
        }
    }

    /// Wrap in `Value::Fn(Arc::new(self))`.
    pub fn into_value(self) -> Value {
        Value::Fn(Arc::new(self))
    }
}
