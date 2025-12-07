# Installation

Add `event-sourcing` to your `Cargo.toml`:

```toml
[dependencies]
event-sourcing = "0.1"
serde = { version = "1", features = ["derive"] }
```

The crate re-exports the derive macro, so you don't need a separate dependency for `event-sourcing-macros`.

## Feature Flags

| Feature | Description |
|---------|-------------|
| `test-util` | Enables `TestFramework` for given-when-then aggregate testing |

To enable test utilities:

```toml
[dev-dependencies]
event-sourcing = { version = "0.1", features = ["test-util"] }
```

## Minimum Rust Version

This crate requires **Rust 1.88.0** or later (edition 2024).

## Next

[Quick Start](quickstart.md) — Build your first aggregate
