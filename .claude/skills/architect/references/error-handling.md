# Error Handling with thiserror and eyre

## When to Use What

### Use `thiserror` for:
- **Libraries** - Expose structured errors to consumers
- **Type-safe errors** - Specific error variants with context
- **Error hierarchies** - Different error types that need conversion
- **Public APIs** - When callers need to match on error kinds

### Use `eyre` for:
- **Applications** - Quick error handling with context
- **Prototyping** - Fast iteration without error boilerplate
- **CLI tools** - Rich error reports for end users
- **Scripts** - When precise error types don't matter

## thiserror Patterns

### Basic Error Enum

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum DataError {
    #[error("file not found: {path}")]
    NotFound { path: String },

    #[error("invalid data at line {line}: {msg}")]
    Invalid { line: usize, msg: String },

    #[error("io error")]
    Io(#[from] std::io::Error),

    #[error("parse error")]
    Parse(#[from] serde_json::Error),
}
```

Key features:
- `#[error("...")]` - Display message with field interpolation
- `#[from]` - Automatic conversion with `?` operator
- Derive `Debug` for error trait

### Transparent Wrapper

```rust
#[derive(Error, Debug)]
pub enum AppError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    #[error(transparent)]
    Config(#[from] toml::de::Error),
}
```

`#[error(transparent)]` forwards the inner error's Display impl.

### Error with Backtrace

```rust
#[derive(Error, Debug)]
pub enum Error {
    #[error("operation failed")]
    Failed {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        backtrace: std::backtrace::Backtrace,
    },
}
```

### Non-Error Sources

```rust
#[derive(Error, Debug)]
pub enum ApiError {
    #[error("request failed: {status}")]
    RequestFailed {
        status: u16,
        #[source]  // Mark as source even though u16 isn't an Error
        body: String,
    },
}
```

## eyre Patterns

### Basic Usage

```rust
use eyre::{Result, eyre, bail, ensure, Context};

fn process_file(path: &str) -> Result<Data> {
    let contents = std::fs::read_to_string(path)
        .wrap_err("failed to read config file")?;

    let data: Data = serde_json::from_str(&contents)
        .wrap_err_with(|| format!("failed to parse {}", path))?;

    ensure!(data.is_valid(), "data validation failed");

    Ok(data)
}

fn main() -> Result<()> {
    if !check_precondition() {
        bail!("precondition not met");
    }

    process_file("config.json")?;
    Ok(())
}
```

Key functions:
- `wrap_err()` - Add context string
- `wrap_err_with()` - Add context with closure (lazy)
- `ensure!()` - Return error if condition false
- `bail!()` - Early return with error
- `eyre!()` - Create error value

### Custom Context

```rust
use eyre::{Result, WrapErr};

fn load_config() -> Result<Config> {
    let path = "config.toml";

    std::fs::read_to_string(path)
        .wrap_err_with(|| format!("reading {}", path))?
        .parse()
        .wrap_err_with(|| format!("parsing {}", path))
}
```

### Rich Error Reports

```rust
use eyre::Result;

fn main() -> Result<()> {
    // Install default error handler for pretty reports
    color_eyre::install()?;

    run()?;
    Ok(())
}
```

With color_eyre, errors show:
- Cause chain
- Spantrace (async context)
- Environment info
- Suggestions

## Combining Both

### Library with thiserror, app with eyre

```rust
// lib.rs - Library exports structured errors
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LibError {
    #[error("database error")]
    Database(#[from] sqlx::Error),

    #[error("not found: {0}")]
    NotFound(String),
}

// main.rs - Application uses eyre
use eyre::{Result, WrapErr};

fn main() -> Result<()> {
    color_eyre::install()?;

    my_lib::process()
        .wrap_err("failed to process")?;

    Ok(())
}
```

### Convert between them

```rust
use eyre::Result;

fn app_logic() -> Result<()> {
    match lib_function() {
        Ok(val) => Ok(val),
        Err(LibError::NotFound(key)) => {
            // Handle specific error
            log::warn!("Missing key: {}", key);
            Ok(())
        }
        Err(e) => {
            // Convert other errors to eyre
            Err(e).wrap_err("lib operation failed")
        }
    }
}
```

## Error Context Best Practices

### Bad: Generic messages
```rust
std::fs::read("config.toml")
    .wrap_err("read failed")?;  // ❌ Not helpful
```

### Good: Specific context
```rust
std::fs::read("config.toml")
    .wrap_err("failed to read application config from config.toml")?;
```

### Bad: Redundant context
```rust
parse_config(&contents)
    .wrap_err("failed to parse config")?;  // ❌ parse_config error already says this
```

### Good: Add new information
```rust
parse_config(&contents)
    .wrap_err_with(|| format!("config must be valid TOML (found in {})", path))?;
```

## Anyhow vs Eyre

Both are similar, eyre adds:
- Better error reports with color_eyre
- Hook system for customization
- Spantrace for async context

Choose eyre for applications, anyhow also fine. For libraries, use thiserror.

## Pattern: Result Type Alias

```rust
// Library: Export custom Result type
pub type Result<T> = std::result::Result<T, Error>;

pub fn load() -> Result<Config> {
    // Uses our Error type by default
}
```

```rust
// Application: Use eyre::Result everywhere
use eyre::Result;  // Import once

fn foo() -> Result<()> { Ok(()) }
fn bar() -> Result<String> { Ok("".into()) }
```

## Recovery Strategies

```rust
use eyre::{Result, eyre};

fn with_retry() -> Result<Data> {
    for attempt in 1..=3 {
        match fetch_data() {
            Ok(data) => return Ok(data),
            Err(e) if attempt < 3 => {
                log::warn!("Attempt {} failed: {}", attempt, e);
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => return Err(e).wrap_err("all retry attempts failed"),
        }
    }

    Err(eyre!("unreachable"))
}
```

## Testing Error Cases

```rust
#[test]
fn test_error_message() {
    let err = process_invalid().unwrap_err();

    // With thiserror: match on variants
    assert!(matches!(err, DataError::Invalid { .. }));

    // With eyre: check message
    assert!(err.to_string().contains("invalid data"));
}
```
