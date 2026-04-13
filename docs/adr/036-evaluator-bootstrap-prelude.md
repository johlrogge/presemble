# ADR-036: Bootstrap evaluator standard library from core.clj prelude

## Status

Accepted

## Context

The evaluator today is a monolithic match statement in `components/evaluator/src/lib.rs`. Every function -- `map`, `filter`, `sort-by`, `get`, arithmetic, string operations, conductor queries -- is a hardcoded Rust function dispatched by name in a single `match func_name { ... }` block (~800 lines). There is no environment, no user-defined functions, no lexical scope, and no way to extend the language without modifying Rust code.

Additionally, the `Value` enum in `template::data` lacks proper types for integers, booleans, and keywords. Integers are stored as `Value::Text("42")`, booleans as `Value::Text("true")`, and keywords are not representable as values at all -- bare `:post` triggers a side-effecting stem query rather than evaluating to a keyword value. This makes arithmetic parse-heavy, equality comparisons unreliable, and higher-order programming impossible.

The nREPL server (`components/nrepl/src/lib.rs`) forwards code strings through the `NreplHandler` trait to the conductor, which calls `evaluator::eval_str`. There is no per-session state because there is no environment to hold it.

The macro expander (`components/macros/src/lib.rs`) handles only `->` and `->>`. Common Clojure patterns (`when`, `if-let`, `cond`, `and`, `or`, threading variants) are unavailable.

This ADR addresses all of these together because they are coupled: user-defined functions require an environment, which requires proper value types (closures capture environments), which requires the keyword semantic change (bare keywords must be values for closures and `def` to work), and the prelude requires all of the above.

### Related ADRs

- **ADR-031** (Conductor as sole authority): The evaluator is invoked through the conductor. The environment's root bindings for conductor-specific functions (`query`, `get-content`, `suggest`, etc.) require a conductor reference, which means primitive functions that touch the conductor remain in Rust.
- **ADR-021** (Persistent DOM trees): Values use `im::HashMap` for structural sharing. The new `Env` will use the same crate.

## Decision

### 1. Value type additions

Add four variants to `template::Value`:

- `Value::Integer(i64)` -- proper numeric type, eliminating string-parsing in arithmetic
- `Value::Bool(bool)` -- proper boolean, eliminating `"true"`/`"false"` string comparisons
- `Value::Keyword { namespace: Option<String>, name: String }` -- keywords as first-class values
- `Value::Fn(Arc<dyn Callable>)` -- closures and primitive functions behind a trait

Where `Callable` is a trait defined in `template::data`:

```rust
pub trait Callable: Send + Sync {
    fn call(&self, args: Vec<Value>) -> Result<Value, String>;
    fn name(&self) -> Option<&str>;
}
```

Multi-arity: `(fn ([x] body1) ([x y] body2))` stores as a list of `FnArity` entries.
Variadic: `(fn [x & rest] body)` -- `&` marks the rest parameter.

### 2. Environment

`Env` struct with:
- `bindings: im::HashMap<String, Value>` -- current scope's bindings
- `parent: Option<Arc<Env>>` -- lexical parent scope

The root environment is wrapped in `Arc<RwLock<Env>>` to allow `def` to mutate the top-level namespace. All lexical child scopes (created by `let`, `fn`) are immutable `Arc<Env>` snapshots -- no mutation, no locking.

Symbol resolution walks the parent chain. `def` always writes to the root.

### 3. Special forms (in evaluator, not macros)

These are handled directly by `eval_expanded`, not dispatched through the environment:

- `fn` -- creates a closure capturing the current environment
- `def` -- binds a value in the root environment
- `let` -- sequential binding with a new child scope
- `if` -- conditional; `nil` and `false` are falsy, everything else truthy
- `do` -- evaluate forms in sequence, return last
- `quote` -- return form as data (as a Value)
- `recur` -- tail-call to the enclosing `fn` (error if not in tail position)

`defn` is sugar, expanded by the macro expander: `(defn f [x] body)` → `(def f (fn f [x] body))`.

### 4. Macro expander additions

All implemented as Form-to-Form transforms in Rust (no `defmacro` yet):

- Threading variants: `as->`, `cond->`, `cond->>`, `some->`, `some->>`
- Conditionals: `when`, `when-not`, `if-not`, `if-let`, `when-let`, `cond`
- Logical: `and`, `or`
- Definition: `defn` (expands to `def` + `fn`)

### 5. Primitive functions

Move from the hardcoded `match func_name` dispatch to environment-registered functions. Each primitive is bound in the root environment at startup via the `Callable` trait.

**Pure primitives** (no conductor): `=`, `<`, `>`, `+`, `-`, `*`, `/`, `mod`, `not`, `get`, `assoc`, `dissoc`, `conj`, `cons`, `concat`, `first`, `rest`, `count`, `nth`, `contains?`, `apply`, `map`, `filter`, `reduce`, `reduce-kv`, `sort-by`, `str`, `subs`, `type`, `name`, `namespace`, `keyword`, `symbol`, `map-indexed`, `range`, `repeat`, `vec`, `hash-map`, `set`, `merge`, `keys`, `vals`.

**Conductor primitives** (require conductor reference): `query`, `get-content`, `get-schema`, `list-content`, `list-schemas`, `refs-to`, `refs-from`, `suggest`, `get-suggestions`, `println`, `doc`.

### 6. Keyword semantics migration

Today: `:post` evaluates to a side-effecting stem query.
After: `:post` evaluates to `Value::Keyword { namespace: None, name: "post" }`.

Stem queries move to an explicit function: `(query :post)`.

Keyword-in-function-position remains: `(:title record)` is sugar for `(get record :title)`.

### 7. Prelude loading

A `core.clj` file is embedded in the evaluator component via `include_str!`. On environment creation, the evaluator reads and evaluates `core.clj` into the root environment after all Rust primitives are registered.

The prelude defines ~50 functions in terms of primitives: `identity`, `constantly`, `comp`, `partial`, `juxt`, `complement`, `fnil`, `every-pred`, `some-fn`, `nil?`, `some?`, `inc`, `dec`, `zero?`, `pos?`, `neg?`, `even?`, `odd?`, `min`, `max`, `abs`, `remove`, `mapcat`, `keep`, `group-by`, `frequencies`, `zipmap`, `flatten`, `distinct`, `interpose`, `assoc-in`, `update`, `update-in`, `select-keys`, `merge-with`, `empty?`, `not-empty`, type predicates, string utilities, etc.

### 8. Deferred: `defmacro`

User-defined macros are not included in this phase. All macros are hardcoded Form-to-Form transforms in the Rust macro expander. This keeps the macro system predictable and avoids the complexity of macro hygiene and expansion-time evaluation.

## Consequences

### Positive

- **Extensibility without recompilation.** New functions can be defined in `core.clj` or at the REPL without touching Rust code.
- **Proper types.** `(+ 1 2)` returns `Value::Integer(3)`, not `Value::Text("3")`. Equality works on values, not string representations.
- **Composability.** User-defined functions, closures, and higher-order functions enable pipeline-style data exploration in the REPL.
- **Smaller evaluator.** The monolithic match statement shrinks to special-form handling. Primitives are regular environment entries.

### Negative

- **Migration surface.** Adding `Value::Integer`, `Value::Bool`, `Value::Keyword`, and `Value::Fn` to the enum requires updating every exhaustive match across multiple files. This is mechanical but wide.
- **Breaking change to keyword semantics.** Any nREPL session or script using bare `:post` for stem queries must change to `(query :post)`. This is a small user base (internal only) but must be communicated.
- **Startup cost.** Evaluating `core.clj` at conductor creation adds startup time. Mitigated by `include_str!` (no I/O) and the prelude being small (~100 defs).
- **Two function representations.** Closures and primitive functions both implement `Callable`. The evaluator must handle both in function application. This is standard for interpreted languages but adds a code path.

### Risks

- **`im` dependency for Env.** Already used by `DataGraph` and `content`, so no new dependency -- but `Env` creates a new high-frequency allocation path. Profile if REPL responsiveness degrades.
- **`recur` in tail position only.** Enforcing tail-position-only `recur` requires a simple static check or a runtime check. Start with runtime (error on non-tail recur) and add static analysis later.
- **Circular dependencies.** `Value::Fn` cannot hold evaluator types directly (template cannot depend on evaluator). The `Callable` trait in template with `Arc<dyn Callable>` follows dependency inversion and keeps polylith component boundaries clean.
