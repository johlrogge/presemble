# Rust Patterns

## Newtype Pattern

Wrap primitives to prevent value mixing and add type safety:

```rust
struct UserId(u64);
struct OrderId(u64);

// Compiler prevents: let user = UserId(order.0);
```

**When to use liberally:**
- Avoiding magic numbers
- Preventing unit confusion (meters vs feet)
- Preventing ID mixing (user_id vs order_id)
- Adding domain meaning to primitives

## Typestate Pattern

Encode state transitions in the type system:

```rust
struct Locked;
struct Unlocked;

struct Door<State> {
    _state: PhantomData<State>,
}

impl Door<Locked> {
    fn unlock(self) -> Door<Unlocked> { /* ... */ }
}

impl Door<Unlocked> {
    fn lock(self) -> Door<Locked> { /* ... */ }
    fn open(&mut self) { /* ... */ }
}
```

**Use to make illegal states unrepresentable:**
- State machines
- Builder patterns with required fields
- Protocol implementations

## Builder Pattern

### Classic Builder
```rust
#[derive(Default)]
struct ConfigBuilder {
    host: Option<String>,
    port: Option<u16>,
}

impl ConfigBuilder {
    fn host(mut self, host: String) -> Self {
        self.host = Some(host);
        self
    }

    fn build(self) -> Result<Config, Error> {
        Ok(Config {
            host: self.host.ok_or(Error::MissingHost)?,
            port: self.port.unwrap_or(8080),
        })
    }
}
```

### Typestate Builder (Compile-time Guarantees)
```rust
struct NoHost;
struct WithHost(String);

struct ConfigBuilder<H> {
    host: H,
    port: u16,
}

impl ConfigBuilder<NoHost> {
    fn new() -> Self {
        Self { host: NoHost, port: 8080 }
    }

    fn host(self, host: String) -> ConfigBuilder<WithHost> {
        ConfigBuilder { host: WithHost(host), port: self.port }
    }
}

impl ConfigBuilder<WithHost> {
    fn build(self) -> Config {
        Config { host: self.host.0, port: self.port }
    }
}
```

## Extension Trait Pattern

Add methods to foreign types:

```rust
trait ResultExt<T, E> {
    fn log_error(self) -> Result<T, E>;
}

impl<T, E: Display> ResultExt<T, E> for Result<T, E> {
    fn log_error(self) -> Result<T, E> {
        if let Err(ref e) = self {
            log::error!("Error: {}", e);
        }
        self
    }
}
```

## Visitor Pattern (with traits)

```rust
trait Visitor {
    fn visit_number(&mut self, n: i64);
    fn visit_string(&mut self, s: &str);
}

enum Value {
    Number(i64),
    String(String),
}

impl Value {
    fn accept(&self, visitor: &mut dyn Visitor) {
        match self {
            Value::Number(n) => visitor.visit_number(*n),
            Value::String(s) => visitor.visit_string(s),
        }
    }
}
```

## RAII (Resource Acquisition Is Initialization)

Use Drop for cleanup guarantees:

```rust
struct FileGuard {
    file: File,
}

impl Drop for FileGuard {
    fn drop(&mut self) {
        // Always called, even on panic
        self.file.flush().ok();
    }
}
```

## Interior Mutability Patterns

### RefCell for single-threaded
```rust
use std::cell::RefCell;

struct Cache {
    data: RefCell<HashMap<String, String>>,
}

impl Cache {
    fn get(&self, key: &str) -> Option<String> {
        self.data.borrow().get(key).cloned()
    }
}
```

### Arc<Mutex<T>> for multi-threaded
```rust
use std::sync::{Arc, Mutex};

type SharedCache = Arc<Mutex<HashMap<String, String>>>;
```

### RwLock for read-heavy workloads
```rust
use std::sync::{Arc, RwLock};

let data = Arc::new(RwLock::new(vec![1, 2, 3]));
let read = data.read().unwrap(); // Multiple readers
let write = data.write().unwrap(); // Exclusive writer
```

## Strategy Pattern

```rust
trait CompressionStrategy {
    fn compress(&self, data: &[u8]) -> Vec<u8>;
}

struct GzipStrategy;
struct ZstdStrategy;

impl CompressionStrategy for GzipStrategy { /* ... */ }
impl CompressionStrategy for ZstdStrategy { /* ... */ }

struct Compressor {
    strategy: Box<dyn CompressionStrategy>,
}
```

## Type-Driven API Design

Make invalid states unrepresentable:

```rust
// Bad: Can construct invalid state
struct User {
    email: Option<String>,
    email_verified: bool,
}

// Good: Impossible to have verified without email
enum EmailState {
    Unverified,
    Verified(String),
}

struct User {
    email: EmailState,
}
```
