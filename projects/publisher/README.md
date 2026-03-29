# publisher

The `presemble` CLI binary.

A thin entry point that depends only on `publisher_cli`. All logic lives in `publisher_cli` so it can be tested without binary overhead.

## Build

```
cargo build -p publisher
cargo run -p publisher -- build site/
cargo run -p publisher -- serve site/
cargo run -p publisher -- lsp site/
```

## Install from source

```
cargo install --path projects/publisher
```

---

[Back to root README](../../README.md)
