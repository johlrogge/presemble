# Type-Driven Design: Make Illegal States Unrepresentable

## Core Philosophy

**If it compiles, it works.** Design types so invalid states cannot be constructed.

## Pattern: Eliminate Invalid Combinations

### Bad: Boolean Flags
```rust
// ❌ 4 possible states, only 3 valid
struct Connection {
    connected: bool,
    authenticated: bool,  // Can be true when connected is false!
}
```

### Good: State Enum
```rust
// ✅ Only 3 constructible states
enum Connection {
    Disconnected,
    Connected,
    Authenticated,
}
```

### Bad: Optional Dependencies
```rust
// ❌ Can have email_verified without email
struct User {
    email: Option<String>,
    email_verified: bool,
}
```

### Good: Nested Types
```rust
// ✅ Can't verify without email
enum EmailState {
    None,
    Unverified(String),
    Verified(String),
}

struct User {
    email: EmailState,
}
```

## Pattern: Builder with Typestate

### Bad: Runtime Validation
```rust
// ❌ Can build invalid config
#[derive(Default)]
struct Config {
    host: Option<String>,
    port: Option<u16>,
}

impl Config {
    fn build(self) -> Result<ValidConfig, Error> {
        Ok(ValidConfig {
            host: self.host.ok_or(Error::MissingHost)?,
            port: self.port.ok_or(Error::MissingPort)?,
        })
    }
}
```

### Good: Compile-time Guarantees
```rust
// ✅ Cannot build without required fields
struct NoHost;
struct HasHost;
struct NoPort;
struct HasPort;

struct ConfigBuilder<H, P> {
    host: H,
    port: P,
}

impl ConfigBuilder<NoHost, NoPort> {
    fn new() -> Self {
        Self { host: NoHost, port: NoPort }
    }
}

impl<P> ConfigBuilder<NoHost, P> {
    fn host(self, host: String) -> ConfigBuilder<HasHost, P> {
        ConfigBuilder { host: HasHost(host), port: self.port }
    }
}

impl<H> ConfigBuilder<H, NoPort> {
    fn port(self, port: u16) -> ConfigBuilder<H, HasPort> {
        ConfigBuilder { host: self.host, port: HasPort(port) }
    }
}

impl ConfigBuilder<HasHost, HasPort> {
    fn build(self) -> Config {
        Config {
            host: self.host.0,
            port: self.port.0,
        }
    }
}

// Usage: Must call host() and port() before build()
let config = ConfigBuilder::new()
    .host("localhost".into())
    .port(8080)
    .build();  // Only available after both set
```

## Pattern: Non-Empty Collections

### Bad: Runtime Checks
```rust
// ❌ Can be empty
fn average(nums: Vec<i32>) -> f64 {
    if nums.is_empty() {
        panic!("Cannot average empty list");
    }
    nums.iter().sum::<i32>() as f64 / nums.len() as f64
}
```

### Good: Non-Empty Type
```rust
// ✅ Guaranteed non-empty
struct NonEmpty<T> {
    head: T,
    tail: Vec<T>,
}

impl<T> NonEmpty<T> {
    fn new(head: T) -> Self {
        Self { head, tail: vec![] }
    }

    fn push(&mut self, item: T) {
        self.tail.push(item);
    }

    fn len(&self) -> usize {
        1 + self.tail.len()
    }
}

fn average(nums: NonEmpty<i32>) -> f64 {
    let sum: i32 = std::iter::once(nums.head)
        .chain(nums.tail.iter().copied())
        .sum();
    sum as f64 / nums.len() as f64
}
```

## Pattern: Validated Data

### Bad: Stringly-Typed
```rust
// ❌ Can pass invalid email
fn send_email(to: String, subject: String, body: String) {
    // Hope 'to' is valid!
}
```

### Good: Validated Newtype
```rust
// ✅ Can only construct with valid email
pub struct Email(String);

impl Email {
    pub fn new(s: String) -> Result<Self, EmailError> {
        if s.contains('@') && s.contains('.') {
            Ok(Email(s))
        } else {
            Err(EmailError::Invalid)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn send_email(to: Email, subject: String, body: String) {
    // Guaranteed valid email
}
```

## Pattern: Units and Dimensions

### Bad: Primitive Confusion
```rust
// ❌ Can mix meters and feet
fn compute_area(width: f64, height: f64) -> f64 {
    width * height  // What units?
}
```

### Good: Newtype Units
```rust
// ✅ Cannot mix units
struct Meters(f64);
struct Feet(f64);

impl Meters {
    fn to_feet(&self) -> Feet {
        Feet(self.0 * 3.28084)
    }
}

fn compute_area(width: Meters, height: Meters) -> SquareMeters {
    SquareMeters(width.0 * height.0)
}
```

## Pattern: Protocol States

### Bad: State Machine as Enum + Data
```rust
// ❌ Can have wrong data for state
struct Connection {
    state: State,
    socket: Option<TcpStream>,
    session: Option<Session>,
}

enum State {
    Disconnected,
    Connected,
    Authenticated,
}
```

### Good: State as Type Parameter
```rust
// ✅ Each state has correct data
struct Disconnected;
struct Connected { socket: TcpStream }
struct Authenticated { socket: TcpStream, session: Session }

struct Connection<S> {
    state: S,
}

impl Connection<Disconnected> {
    fn new() -> Self {
        Self { state: Disconnected }
    }

    fn connect(self, addr: SocketAddr) -> io::Result<Connection<Connected>> {
        let socket = TcpStream::connect(addr)?;
        Ok(Connection { state: Connected { socket } })
    }
}

impl Connection<Connected> {
    fn authenticate(self, creds: Credentials) -> Result<Connection<Authenticated>> {
        let session = auth::login(creds)?;
        Ok(Connection {
            state: Authenticated {
                socket: self.state.socket,
                session,
            }
        })
    }
}

impl Connection<Authenticated> {
    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.state.socket.write_all(data)
    }
}
```

## Pattern: Parse, Don't Validate

### Bad: Validate Then Use
```rust
// ❌ Validation separate from parsing
fn is_valid_port(s: &str) -> bool {
    s.parse::<u16>().is_ok()
}

fn use_port(s: &str) {
    if !is_valid_port(s) {
        panic!("invalid");
    }
    let port = s.parse::<u16>().unwrap();  // Parsing twice!
}
```

### Good: Parse Once
```rust
// ✅ Validation IS parsing
fn parse_port(s: &str) -> Result<u16, ParseError> {
    s.parse()
}

fn use_port(s: &str) {
    match parse_port(s) {
        Ok(port) => {
            // Use port, guaranteed valid
        }
        Err(e) => {
            // Handle error
        }
    }
}
```

## Pattern: Smart Constructors

```rust
// ✅ Constructor enforces invariants
pub struct PositiveInt(i32);

impl PositiveInt {
    pub fn new(n: i32) -> Option<Self> {
        if n > 0 {
            Some(PositiveInt(n))
        } else {
            None
        }
    }

    pub fn get(&self) -> i32 {
        self.0
    }
}

// Cannot construct invalid PositiveInt
// Fields are private, must use constructor
```

## Benefits

1. **Fewer tests** - Invalid states cannot be constructed
2. **Better refactoring** - Compiler catches usage errors
3. **Self-documenting** - Types encode constraints
4. **Reduced cognitive load** - No runtime checks needed
5. **Fearless concurrency** - Send/Sync bounds prevent data races

## When to Use

- **Always** consider: Can this state be invalid?
- Use when invalid states have serious consequences
- Use when APIs are public/stable
- Use when domain has clear invariants
- Balance complexity vs safety for your use case
