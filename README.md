# event-sourcing

Building blocks for pragmatic event-sourced systems in Rust. The crate focuses on keeping
domain types pure while giving you the tools to rebuild state, project read models, and
persist events through a pluggable store interface.

## Highlights

- **Domain-first API** – events are plain structs that implement `DomainEvent`; IDs and
  metadata live in an envelope rather than in your payloads.
- **Aggregate derive** – `#[derive(Aggregate)]` generates the event enum plus
  serialization/deserialization glue so command handlers stay focused on behaviour.
- **Repository orchestration** – `Repository` loads aggregates, executes commands via
  `Handle<C>`, and persists the resulting events in a single transaction.
- **Metadata-aware projections** – projections receive `EventEnvelope` values so they can
  correlate cross-aggregate workflows using IDs, causation, timestamps, or custom metadata.
- **Store agnostic** – ships with an in-memory store for demos/tests; implement the
  `EventStore` trait to plug in your own persistence.

## How this differs from other Rust event-sourcing crates

This crate borrows inspiration from projects like
[`eventually`](https://github.com/get-eventually/eventually-rs) and
[`cqrs`](https://github.com/serverlesstechnology/cqrs) but makes a few different trade-offs:

- **Events stay as first-class structs.** Instead of immediately wrapping events in
  aggregate-specific enums, each `DomainEvent` stands on its own. Multiple aggregates (or
  even completely unrelated subsystems) can reuse the same event type. Projections receive
  `EventEnvelope`s that carry aggregate identifiers and metadata rather than relying on
  the payload to embed IDs.

- **Projections are fully decoupled.** Read models don’t have to depend on a particular
  aggregate enum or repository type. You declare the events you care about—potentially
  pulling from several aggregate kinds—and compose them via the builder. The fluent
  `ProjectionBuilder` keeps common cases ergonomic while still leaving room for custom
  loading logic when you need it.

- **Metadata lives outside domain objects.** Infrastructure concerns (aggregate kind, ID,
  causation/correlation IDs, user info) travel alongside the event via `EventEnvelope`. The
  domain event itself remains pure, making it easier to share across bounded contexts.

- **More boilerplate, mitigated when it matters.** Because events and projections are
  explicit structs, the type definitions are a bit louder than frameworks that lean on
  trait objects or dynamic dispatch. The provided `#[derive(Aggregate)]` covers the
  command side, while projections stay explicit and lean on the builder to avoid duplicate
  wiring.

- **Minimal infrastructure baked in.** There is no built-in command bus, outbox, snapshot
  scheduler, or event streaming layer. You wire the repository into whichever pipeline you
  prefer. That keeps the crate lightweight compared to libraries that bundle an entire
  CQRS stack.

- **Versioning happens at the codec layer.** We don’t expose an explicit “upcaster” concept
  like `cqrs`. Instead, you can migrate historical events transparently inside your codec
  (see [`examples/versioned_events.rs`](examples/versioned_events.rs)).

- **No optimistic mutation hooks (yet).** `eventually` ships batteries-included APIs for
  version-checked mutations. This crate intentionally keeps the repository/store layer lean
  and does not currently expose an optimistic concurrency hook. Adding that capability is on
  the roadmap; contributions welcome.

## Quick start

The snippet below wires together a single aggregate, a projection, and a repository using
the aggregate derive and the in-memory store.

```rust,no_run
use event_sourcing::{
    Apply, ApplyProjection, DomainEvent, EventMetadata, Handle, InMemoryEventStore, JsonCodec,
    Projection, Repository,
};
use serde::{Deserialize, Serialize};

// === Domain events ===

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FundsDeposited {
    pub amount_cents: i64,
}

impl DomainEvent for FundsDeposited {
    const KIND: &'static str = "bank.account.deposited";
}

// === Commands ===

#[derive(Debug)]
pub struct DepositFunds {
    pub amount_cents: i64,
}

// === Aggregate ===

#[derive(Debug, Default, event_sourcing::Aggregate)]
#[aggregate(
    id = String,
    error = String,
    events(FundsDeposited),
    kind = "bank-account"
)]
pub struct Account {
    balance_cents: i64,
}

impl Apply<FundsDeposited> for Account {
    fn apply(&mut self, event: &FundsDeposited) {
        self.balance_cents += event.amount_cents;
    }
}

impl Handle<DepositFunds> for Account {
    fn handle(&self, command: &DepositFunds) -> Result<Vec<Self::Event>, Self::Error> {
        if command.amount_cents <= 0 {
            return Err("amount must be positive".to_string());
        }
        Ok(vec![FundsDeposited {
            amount_cents: command.amount_cents,
        }
        .into()])
    }
}

// === Projection ===

#[derive(Debug, Default)]
pub struct AccountBalance {
    pub total_cents: i64,
}

impl Projection for AccountBalance {
    type Metadata = EventMetadata;
}

impl ApplyProjection<FundsDeposited, EventMetadata> for AccountBalance {
    fn apply_projection(
        &mut self,
        _aggregate_id: &str,
        event: &FundsDeposited,
        _metadata: &EventMetadata,
    ) {
        self.total_cents += event.amount_cents;
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store: InMemoryEventStore<JsonCodec, EventMetadata> = InMemoryEventStore::new(JsonCodec);
    let mut repository = Repository::new(store);

    let account_id = "ACC-001".to_string();
    let command = DepositFunds { amount_cents: 25_00 };
    repository.execute_command::<Account, DepositFunds>(
        &account_id,
        &command,
        &EventMetadata::default(),
    )?;

    let summary = repository
        .build_projection::<AccountBalance>()
        .event::<FundsDeposited>()
        .load()?;
    assert_eq!(summary.total_cents, 25_00);
    Ok(())
}
```

See [`examples/`](examples) for larger, end-to-end scenarios (composite IDs, CQRS dashboards,
versioned events, etc.).

## Core concepts

### Aggregates

Aggregates rebuild state from events and validate commands. Implement `Apply<E>` for each
event you care about, then add `Handle<C>` implementations for each command. The
`#[derive(Aggregate)]` macro generates the sum-type that glues everything together.

### Projections

Read models that keep their state in sync by replaying events. Projections implement
`ApplyProjection<E, M>` for the event/metadata combinations they care about and declare their
metadata requirements via the `Projection` trait. Build them by calling
`Repository::build_projection::<P>()`, chaining the relevant `.event::<E>()` / `.event_for::<A, E>()`
registrations, and finally `.load()`.

### Event envelope

The repository loads raw events from the store, converts them into `EventEnvelope` values,
and dispatches them to projections. The envelope includes:

- `aggregate_kind` – matches `Aggregate::KIND`
- `aggregate_id` – the instance identifier (without the kind prefix)
- `metadata` – timestamps, causation IDs, user information, or your own metadata type

Aggregates never see the envelope—only the pure events.

### Repositories & stores

`Repository<S>` orchestrates aggregate loading, command execution, and appending to the
underlying store. The `EventStore` trait defines the persistence boundary; implement it to
back the repository with Postgres, `DynamoDB`, S3, or anything else that can read/write ordered
event streams.

## Running the examples

```shell
cargo run --example inventory_report
cargo run --example subscription_billing
cargo run --example versioned_events
cargo run --example advanced_projection
```

## Status

The crate is still pre-1.0. Expect APIs to evolve as real-world usage grows. Feedback and
contributions are welcome! Submit an issue or pull request if you spot something missing. 
