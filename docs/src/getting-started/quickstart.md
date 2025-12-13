# Quick Start

Build a simple bank account aggregate in under 50 lines. This example demonstrates events, commands, an aggregate, a projection, and the repository.

## The Complete Example

```rust,ignore
{{#include ../../../examples/quickstart.rs:full_example}}
```

## What Just Happened?

1. **Defined an event** — `FundsDeposited` is a simple struct with `DomainEvent`
2. **Defined a command** — `Deposit` is a plain struct (no traits required)
3. **Created an aggregate** — `Account` uses the derive macro to generate boilerplate
4. **Implemented `Apply`** — How events mutate state
5. **Implemented `Handle`** — How commands produce events
6. **Created a projection** — `TotalDeposits` builds a read model
7. **Wired the repository** — Connected everything with `InMemoryEventStore`

## Key Points

- **Events are past tense facts** — `FundsDeposited`, not `DepositFunds`
- **Commands are imperative** — `Deposit`, not `Deposited`
- **The derive macro generates** — The event enum, `From` impls, serialization
- **Projections are decoupled** — They receive events, not aggregate types
- **IDs are infrastructure** — Passed to the repository, not embedded in events

## Next Steps

- [Aggregates](../core-traits/aggregate.md) — Deep dive into the aggregate trait
- [Projections](../core-traits/projections.md) — Multi-stream projections
- [The Aggregate Derive](../derive-macros/aggregate-derive.md) — Full attribute reference
