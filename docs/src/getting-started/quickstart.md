# Quick Start

Build a simple bank account aggregate in under 50 lines. This example demonstrates events, commands, an aggregate, a projection, and the repository.

## The Complete Example

```rust,ignore
use event_sourcing::{
    Apply, ApplyProjection, DomainEvent, Handle, InMemoryEventStore,
    JsonCodec, Projection, Repository,
};
use serde::{Deserialize, Serialize};

// --- Events ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundsDeposited {
    pub amount: i64,
}

impl DomainEvent for FundsDeposited {
    const KIND: &'static str = "account.deposited";
}

// --- Commands ---

#[derive(Debug)]
pub struct Deposit {
    pub amount: i64,
}

// --- Aggregate ---

#[derive(Debug, Default, Serialize, Deserialize, event_sourcing::Aggregate)]
#[aggregate(id = String, error = String, events(FundsDeposited))]
pub struct Account {
    balance: i64,
}

impl Apply<FundsDeposited> for Account {
    fn apply(&mut self, event: &FundsDeposited) {
        self.balance += event.amount;
    }
}

impl Handle<Deposit> for Account {
    fn handle(&self, cmd: &Deposit) -> Result<Vec<Self::Event>, Self::Error> {
        if cmd.amount <= 0 {
            return Err("amount must be positive".into());
        }
        Ok(vec![FundsDeposited { amount: cmd.amount }.into()])
    }
}

// --- Projection ---

#[derive(Debug, Default)]
pub struct TotalDeposits {
    pub total: i64,
}

impl Projection for TotalDeposits {
    type Metadata = ();
}

impl ApplyProjection<FundsDeposited, ()> for TotalDeposits {
    fn apply_projection(&mut self, _id: &str, event: &FundsDeposited, _meta: &()) {
        self.total += event.amount;
    }
}

// --- Wiring it together ---

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create an in-memory store
    let store: InMemoryEventStore<JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);
    let mut repository = Repository::new(store);

    // Execute a command
    repository.execute_command::<Account, Deposit>(
        &"ACC-001".to_string(),
        &Deposit { amount: 100 },
        &(),
    )?;

    // Build a projection
    let totals = repository
        .build_projection::<TotalDeposits>()
        .event::<FundsDeposited>()
        .load()?;

    println!("Total deposits: {}", totals.total); // 100
    Ok(())
}
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
