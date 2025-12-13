# CQRS Overview

**Command Query Responsibility Segregation** (CQRS) separates the write model (how you change data) from the read model (how you query data). Event sourcing and CQRS complement each other naturally.

## The Split

```d2
Write: "Write Side (Commands)" {
  Command -> Aggregate -> Events -> Store: Event Store {
    shape: cylinder
  }
}

Read: "Read Side (Queries)" {
  Store: Event Store {
    shape: cylinder
  }
  Store -> Projection -> View: Read Model {
    shape: cylinder
  }
  View -> Query
}

Write.Store -> Read.Store: {style.stroke-dash: 3}
```

**Write side**: Commands validate against the aggregate and produce events. The aggregate enforces business rules.

**Read side**: Projections consume events and build optimized views for queries. Each projection can structure data however the UI needs.

## Why Separate?

| Concern | Write Model | Read Model |
|---------|-------------|------------|
| **Optimized for** | Consistency, validation | Query performance |
| **Structure** | Reflects domain behavior | Reflects UI needs |
| **Updates** | Transactional | Eventually consistent |
| **Scaling** | Consistent writes | Replicated reads |

A single aggregate might feed multiple projections:

```d2
direction: right

Events: Events {
  shape: cylinder
}

Events -> Account Summary
Events -> Transaction History
Events -> Fraud Detection
Events -> Analytics Dashboard
```

Each projection sees the same events but builds a different view.

## In This Crate

The crate models CQRS through:

- **`Aggregate`** + **`Handle<C>`** — the write side
- **`Projection`** + **`ApplyProjection<E>`** — the read side
- **`Repository`** — orchestrates both

```rust,ignore
// Write: execute a command
repository.execute_command::<Account, Deposit>(&id, &command, &metadata)?;

// Read: build a projection
let summary = repository
    .build_projection::<AccountSummary>()
    .event::<FundsDeposited>()
    .event::<FundsWithdrawn>()
    .load()?;
```

## Eventual Consistency

With CQRS, read models may not immediately reflect the latest write. This is fine for most UIs—users expect slight delays. For operations requiring strong consistency, read from the aggregate itself (via `aggregate_builder`).

## Next

[Architecture](architecture.md) — How the components fit together
