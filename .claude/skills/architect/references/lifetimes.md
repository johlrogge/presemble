# Lifetime Patterns and Solutions

## Common Lifetime Problems

### Problem: Return reference to local
```rust
// ❌ Doesn't compile
fn bad() -> &str {
    let s = String::from("hello");
    &s  // s dropped here
}

// ✅ Return owned data
fn good() -> String {
    String::from("hello")
}

// ✅ Or take output buffer
fn good2(buf: &mut String) {
    buf.push_str("hello");
}
```

### Problem: Multiple borrows conflict
```rust
// ❌ Doesn't compile
fn bad(data: &mut Vec<i32>) {
    let first = &data[0];
    data.push(1);  // Can't mutate while borrowed
    println!("{}", first);
}

// ✅ Clone if needed
fn good(data: &mut Vec<i32>) {
    let first = data[0];  // Copy the value
    data.push(1);
    println!("{}", first);
}

// ✅ Or split borrows
fn good2(data: &mut Vec<i32>) {
    data.push(1);
    let first = &data[0];  // Borrow after mutation
    println!("{}", first);
}
```

### Problem: Self-referential structs
```rust
// ❌ Can't have struct reference its own data
struct Bad<'a> {
    data: String,
    slice: &'a str,  // Can't point into data
}

// ✅ Use indices instead
struct Good {
    data: String,
    slice_start: usize,
    slice_len: usize,
}

impl Good {
    fn slice(&self) -> &str {
        &self.data[self.slice_start..self.slice_start + self.slice_len]
    }
}

// ✅ Or use Pin<Box<T>> with ouroboros crate for complex cases
```

## Lifetime Elision Rules

Rust infers lifetimes in these cases:

1. **Each input reference gets its own lifetime**
   ```rust
   fn foo(x: &i32, y: &i32)
   // Becomes: fn foo<'a, 'b>(x: &'a i32, y: &'b i32)
   ```

2. **Single input lifetime → all outputs get it**
   ```rust
   fn first(x: &Vec<i32>) -> &i32
   // Becomes: fn first<'a>(x: &'a Vec<i32>) -> &'a i32
   ```

3. **Method with &self → outputs get self's lifetime**
   ```rust
   fn get(&self) -> &str
   // Becomes: fn get<'a>(&'a self) -> &'a str
   ```

## Common Patterns

### Pattern: Return one of two inputs
```rust
// Explicit: Both inputs must live as long
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {
    if x.len() > y.len() { x } else { y }
}

// Caller ensures both live long enough
let result = {
    let s1 = String::from("long");
    let s2 = String::from("short");
    longest(&s1, &s2)  // result's lifetime limited to this block
};
```

### Pattern: Return reference tied to specific input
```rust
// Different lifetimes when outputs don't depend on all inputs
fn first<'a, 'b>(x: &'a str, _y: &'b str) -> &'a str {
    x  // Only depends on x's lifetime
}

// Now caller has more flexibility
let result;
{
    let s1 = String::from("first");
    {
        let s2 = String::from("second");
        result = first(&s1, &s2);  // s2 can drop, result still valid
    }
    println!("{}", result);  // Works!
}
```

### Pattern: Struct with references
```rust
struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Parser { input, pos: 0 }
    }

    // Return tied to struct's lifetime
    fn current(&self) -> &'a str {
        &self.input[self.pos..]
    }

    // Method lifetime different from struct lifetime
    fn peek<'b>(&'b self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }
}
```

### Pattern: Multiple lifetime parameters
```rust
struct Context<'a> {
    config: &'a Config,
}

struct Processor<'ctx, 'data> {
    context: &'ctx Context<'ctx>,
    data: &'data [u8],
}

// 'ctx and 'data are independent
impl<'ctx, 'data> Processor<'ctx, 'data> {
    fn process(&self) -> Vec<u8> {
        // Can use both context and data
        self.data.to_vec()
    }
}
```

## Advanced: Higher-Rank Trait Bounds (HRTB)

For functions that work with any lifetime:

```rust
// Accepts any function that works with borrowed data
fn apply<F>(f: F) -> i32
where
    F: for<'a> Fn(&'a str) -> i32,
{
    let s = String::from("hello");
    f(&s)
}

// Works because closure satisfies for<'a>
let len = apply(|s| s.len() as i32);
```

## Pattern: Splitting borrows

```rust
struct Data {
    items: Vec<Item>,
    cache: HashMap<String, usize>,
}

impl Data {
    // ❌ Can't return two &mut to self
    fn bad(&mut self) -> (&mut Vec<Item>, &mut HashMap<String, usize>) {
        (&mut self.items, &mut self.cache)
    }

    // ✅ Return multiple mutable references to fields
    fn split(&mut self) -> (&mut Vec<Item>, &mut HashMap<String, usize>) {
        (&mut self.items, &mut self.cache)
    }
}

// Rust allows splitting because fields don't overlap
```

## Pattern: 'static lifetime

```rust
// String literal is 'static
const MESSAGE: &'static str = "hello";

// Box<dyn Trait + 'static> means no borrowed data
fn process(f: Box<dyn Fn() + 'static>) {
    // Can store f anywhere, even across threads
}

// Owned data satisfies 'static bound
let owned = String::from("owned");
process(Box::new(move || println!("{}", owned)));
```

## Debugging Tips

1. **Start with explicit lifetimes, then remove what compiles**
2. **Use named lifetimes for clarity**
   ```rust
   // Instead of: <'a, 'b>
   // Use: <'input, 'output>
   ```
3. **Draw lifetime diagrams on paper**
4. **Check `cargo expand` to see what compiler sees**
5. **Add `where` clauses to make relationships explicit**
   ```rust
   fn process<'a, 'b>(x: &'a str, y: &'b str) -> &'a str
   where
       'b: 'a,  // 'b outlives 'a
   {
       x
   }
   ```
