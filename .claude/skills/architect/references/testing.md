# Testing in Rust

## Core Philosophy

**Test behaviors, not conformance.** The type system eliminates illegal states and guarantees correctness. Tests verify that correct behaviors emerge from correct states.

## Unit Test Laws

Five non-negotiable rules:

1. **One reason to fail** — each `#[test]` has exactly one `assert!`/`assert_eq!`/`prop_assert!`. Multiple inputs → `rstest #[case]`. Multiple behaviors → multiple tests.

2. **Never trust a test you haven't seen fail** — TDD red-green-refactor. Write the failing test first. `todo!()` counts as seeing it fail.

3. **Simplest code that could possibly work** — if you need complexity, prove it with a failing test first.

4. **No filesystem in unit tests** — no `std::fs`, `File::open`, `tempfile`, `TempDir`. Use `&[u8]`, `Cursor`, or `Path::new("fake.ext")` for extension checks (no I/O).

5. **Unit tests must be fast** — thousands per second. No I/O, no network, no `thread::sleep`.

## Test Grouping

Two tools for grouping tests:

- **rstest `#[case]`** — when multiple tests share the same assertion pattern with different inputs. This is the primary tool for parameterization.
- **submodules** — when breaking up a test that covers multiple *distinct behaviors* of the same subject that cannot be parameterized. Group related single-assert tests under a descriptive submodule.

Submodules are valid for *topic* grouping (different tests, same subject area). rstest replaces *scenario* modules (same test, different inputs) — not topic grouping.

Example of submodule grouping for a state machine:

```rust
#[cfg(test)]
mod tests {
    mod play_queue {
        #[test]
        fn from_idle_state_becomes_playing() { /* ... */ }

        #[test]
        fn from_idle_emits_load_and_play() { /* ... */ }

        #[test]
        fn from_playing_stops_current_then_loads_new() { /* ... */ }
    }

    mod skip {
        #[test]
        fn with_next_track_advances_queue() { /* ... */ }

        #[test]
        fn on_empty_queue_transitions_to_idle() { /* ... */ }
    }
}
```

## Test Naming

### Rule 1: DRY — don't repeat context

- No `test_` prefix on functions inside `mod tests`
- No repeating the submodule name in the function name
- No repeating the type name in associated test methods
- Rename `foo_tests` submodules to just `foo`

```rust
// Bad
mod tests { fn test_playlist_create() { ... } }
mod volume { fn volume_converts_to_linear() { ... } }

// Good
mod tests { fn playlist_create() { ... } }
mod volume { fn converts_to_linear() { ... } }
```

### Rule 2: Name describes the outcome/behavior being verified

The test name states *what should happen*, not *what action is performed*. The submodule provides the action context; the function name states the expected outcome.

```rust
mod tests {
    mod add_track {
        #[test]
        fn does_not_allow_adding_the_same_track_twice() {
            add_track("track1").expect("first add should succeed");
            assert_eq!(add_track("track1"), Err(DuplicateTrack));
        }
    }
}
```

## Assertion Rules

### Use `pretty_assertions`

Add `use pretty_assertions::assert_eq;` in test modules. It produces structured diffs on failure instead of raw Debug output, making it far easier to spot what differs.

### Avoid bare `assert!()`

Prefer `assert_eq!` with an explicit expected value so failures show what was actually received.

### Test collection equality, not length

```rust
// Bad
assert!(my_vec.is_empty());
// Good
assert_eq!(my_vec, vec![]);

// Bad
assert_eq!(results.len(), 2);
// Good
assert_eq!(results, vec![expected_a, expected_b]);
```

### Multiple asserts decision tree

When you see a test with multiple asserts, apply this decision tree:

1. **Function called multiple times with different inputs, output asserted each time** → convert to `rstest #[case]`. Each input/output pair is a separate case.

2. **Function called once, multiple asserts on the output:**
   - **All fields asserted** → replace with a single `assert_eq!` comparing the whole value against an expected value (use a builder if the struct is large)
   - **Only some fields asserted** → keep as selective field assertions, but verify they check the *relevant* fields. This is the one exception to "one assert per test."

```rust
// Pattern 1: multiple inputs → rstest (NOT multiple asserts in one test)
#[rstest]
#[case("track.flac", true)]
#[case("track.mp3", true)]
#[case("cover.jpg", false)]
fn audio_extension_recognition(#[case] filename: &str, #[case] expected: bool) {
    assert_eq!(is_audio_file(Path::new(filename)), expected);
}

// Pattern 2a: all fields → single assert_eq!
#[test]
fn returns_correct_config() {
    assert_eq!(build_config(), Config { host: "localhost", port: 8080 });
}

// Pattern 2b: selective fields on one value — acceptable exception
#[test]
fn returns_the_matching_track() {
    let track = library.search("Init").first();
    assert_eq!(track.artist, "Carbon Based Lifeforms");
    assert_eq!(track.title, "Init");
    // duration, bpm, key — don't care, don't assert
}
```

If you find yourself writing eight field assertions selectively, ask: does this struct need to be this large? Needing many selective field assertions is a design smell.

## Serialization Testing

- Serialize and deserialize are **separate concerns** — never test as round-trip in example-based tests
- A round-trip test hides symmetric bugs (serialize and deserialize both wrong in the same way)
- Use `proptest` for round-trip **invariants** — that's the one place both appear together

Bad:
```rust
#[test]
fn bpm_serialization() {
    let bpm = Bpm::from_f32(125.45).unwrap();
    let json = serde_json::to_string(&bpm).unwrap();
    assert_eq!(json, "12545");
    let back: Bpm = serde_json::from_str(&json).unwrap();
    assert_eq!(back, bpm);  // hides symmetric bugs
}
```

Good:
```rust
#[test]
fn bpm_serializes_as_hundredths() {
    assert_eq!(serde_json::to_string(&Bpm::from_f32(125.45).unwrap()).unwrap(), "12545");
}

#[test]
fn bpm_deserializes_from_hundredths() {
    assert_eq!(serde_json::from_str::<Bpm>("12545").unwrap().as_f32(), 125.45);
}

proptest! {
    #[test]
    fn bpm_roundtrip(hundredths in 2000u32..=99999u32) {
        let bpm = Bpm::try_from(hundredths).unwrap();
        let json = serde_json::to_string(&bpm).unwrap();
        let back: Bpm = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(bpm, back);
    }
}
```

## Testing Tools

### rstest - Parametric Testing

Primary testing tool for parameterized tests and fixtures.

```rust
use rstest::*;

#[rstest]
#[case(1, 2, 3)]
#[case(5, 5, 10)]
#[case(0, 100, 100)]
fn addition(#[case] a: i32, #[case] b: i32, #[case] expected: i32) {
    assert_eq!(a + b, expected);
}
```

**Replaces scenario modules**: Instead of creating submodules for different scenarios of the *same test with different inputs*, use rstest with multiple cases. Submodules remain valid for topic grouping of *different tests on the same subject*.

#### Fixtures with rstest
```rust
#[fixture]
fn sample_user() -> User {
    User {
        id: UserId(1),
        email: "test@example.com".into(),
    }
}

#[rstest]
fn user_has_expected_id(sample_user: User) {
    assert_eq!(sample_user.id, UserId(1));
}
```

### Property-Based Testing with proptest

**Mandated** for round-trip invariants and property verification across the input space.

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn reversing_twice_is_identity(s: String) {
        let reversed_twice = s.chars().rev().collect::<String>()
            .chars().rev().collect::<String>();
        prop_assert_eq!(s, reversed_twice);
    }
}
```

**Rule**: Any type that implements `Serialize + Deserialize` must have a proptest round-trip test.

### Benchmarking with Criterion

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn fibonacci_benchmark(c: &mut Criterion) {
    c.bench_function("fib 20", |b| {
        b.iter(|| fibonacci(black_box(20)))
    });
}

criterion_group!(benches, fibonacci_benchmark);
criterion_main!(benches);
```

## Test Organization

Tests live in a `tests` module within the same file:

```rust
// src/user.rs
pub struct User { /* ... */ }

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use rstest::*;

    #[rstest]
    #[case(UserId(1), "test@example.com")]
    fn user_creation(#[case] id: UserId, #[case] email: &str) {
        let user = User::new(id, email.into());
        assert_eq!(user.id, id);
    }
}
```

## Builder Pattern for Test Fixtures

Builders serve double duty: they set up test state AND construct expected values for `assert_eq!`.

```rust
assert_eq!(
    library.get_track(hash),
    TrackBuilder::new().artist("Carbon Based Lifeforms").title("Init").build()
);
```

## Review Checklist

- [ ] Each `#[test]` has exactly one assert
- [ ] No multi-assert tests where function is called multiple times — use `rstest #[case]`
- [ ] Serialization and deserialization tested separately
- [ ] Round-trip invariants use `proptest`, not example-based tests
- [ ] No filesystem I/O in unit tests
- [ ] No `thread::sleep` or real network calls
- [ ] Test names describe the single behavior being verified
- [ ] New code follows TDD (test written before implementation)
- [ ] No `test_` prefix on functions inside `mod tests`
- [ ] Uses `pretty_assertions::assert_eq!` in test modules
- [ ] Collection assertions compare full contents, not `.len()` or `.is_empty()`
