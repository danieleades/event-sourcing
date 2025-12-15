//! A minimal example demonstrating the core concepts of event-sourcing.
//!
//! Run with: `cargo run --example quickstart`

// NB: the 'ANCHOR's support embedding in mdbook in docs/ directory.

// ANCHOR: full_example
use event_sourcing::{
    Apply, ApplyProjection, DomainEvent, Handle, Projection, Repository,
    store::{InMemoryEventStore, JsonCodec},
};
use serde::{Deserialize, Serialize};

// ANCHOR: events
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundsDeposited {
    pub amount: i64,
}

impl DomainEvent for FundsDeposited {
    const KIND: &'static str = "account.deposited";
}
// ANCHOR_END: events

// ANCHOR: commands
#[derive(Debug)]
pub struct Deposit {
    pub amount: i64,
}
// ANCHOR_END: commands

// ANCHOR: aggregate
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
// ANCHOR_END: aggregate

// ANCHOR: projection
#[derive(Debug, Default)]
pub struct TotalDeposits {
    pub total: i64,
}

impl Projection for TotalDeposits {
    type Id = String;
    type Metadata = ();
}

impl ApplyProjection<FundsDeposited> for TotalDeposits {
    fn apply_projection(&mut self, _id: &Self::Id, event: &FundsDeposited, _meta: &Self::Metadata) {
        self.total += event.amount;
    }
}
// ANCHOR_END: projection

// ANCHOR: main
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create an in-memory store
    let store = InMemoryEventStore::new(JsonCodec);
    let mut repository = Repository::new(store);

    // Execute a command
    repository
        .execute_command::<Account, Deposit>(&"ACC-001".to_string(), &Deposit { amount: 100 }, &())
        .await?;

    // Build a projection
    let totals = repository
        .build_projection::<TotalDeposits>()
        .event::<FundsDeposited>()
        .load()
        .await?;

    println!("Total deposits: {}", totals.total);
    assert_eq!(totals.total, 100);

    Ok(())
}
// ANCHOR_END: main
// ANCHOR_END: full_example
