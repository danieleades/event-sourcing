# Introduction

**event-sourcing** provides building blocks for pragmatic event-sourced systems in Rust. It keeps your domain types pure while giving you tools to rebuild state, project read models, and persist events through a pluggable store interface.

## When to Use Event Sourcing

Event sourcing shines when you need:

- **Complete audit trails** — every state change is recorded
- **Time travel** — reconstruct state at any point in history
- **Decoupled read models** — optimize queries independently of writes
- **Complex domain logic** — model behavior as a sequence of facts

It adds complexity, so consider simpler approaches for CRUD-heavy applications with minimal business logic.

## What This Crate Provides

| Included | Not Included |
|----------|--------------|
| Core traits for aggregates, events, projections | Command bus / message broker |
| Derive macro to reduce boilerplate | Outbox pattern |
| Repository for command execution | Snapshot scheduler |
| In-memory store for testing | Event streaming infrastructure |
| PostgreSQL store (optional) | |
| Test framework (given-when-then) | |

The crate is intentionally minimal. You wire the repository into whichever pipeline you prefer—whether that's a web framework, actor system, or CLI tool.

## A Taste of the API

```rust,ignore
#[derive(Aggregate)]
#[aggregate(id = String, error = String, events(FundsDeposited, FundsWithdrawn))]
pub struct Account {
    balance: i64,
}

impl Handle<Deposit> for Account {
    fn handle(&self, cmd: &Deposit) -> Result<Vec<Self::Event>, Self::Error> {
        Ok(vec![FundsDeposited { amount: cmd.amount }.into()])
    }
}
```

The derive macro generates the event enum and serialization glue. You focus on domain behavior.

## Next Steps

- [Event Sourcing Primer](concepts/event-sourcing-primer.md) — understand the core concepts
- [Quick Start](getting-started/quickstart.md) — build your first aggregate in 5 minutes
- [Core Traits](core-traits/aggregate.md) — deep dive into the API
