# Installation

Add `sourcery` to your `Cargo.toml`:

```toml
[dependencies]
sourcery = "0.1"
serde = { version = "1", features = ["derive"] }
```

The crate re-exports the derive macro, so you don't need a separate dependency for `sourcery-macros`.

## Feature Flags

| Feature | Description |
|---------|-------------|
| `test-util` | Enables `TestFramework` for given-when-then aggregate testing |
| `postgres` | PostgreSQL event store via `sqlx` |

To enable test utilities:

```toml
[dev-dependencies]
sourcery = { version = "0.1", features = ["test-util"] }
```

To use the PostgreSQL store:

```toml
[dependencies]
sourcery = { version = "0.1", features = ["postgres"] }
```

## Minimum Rust Version

This crate requires **Rust 1.88.0** or later (edition 2024).

## Next

[Quick Start](quickstart.md) — Build your first aggregate
