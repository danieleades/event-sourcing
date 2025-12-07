# Design Decisions

This page explains the reasoning behind key architectural choices in the crate.

## Events as First-Class Structs

**Decision**: Events are standalone structs implementing `DomainEvent`, not just enum variants.

**Why**:
- Events can be shared across multiple aggregates
- Projections subscribe to event types, not aggregate enums
- Easier to evolve events independently
- Better compile-time isolation

**Trade-off**: More boilerplate (mitigated by derive macro).

```rust,ignore
// This crate: events stand alone
struct OrderPlaced { /* ... */ }
struct PaymentReceived { /* ... */ }

// Both Order and Payment aggregates can use PaymentReceived
```

## IDs as Infrastructure

**Decision**: Aggregate IDs are passed to the repository, not embedded in events or handlers.

**Why**:
- Domain events stay pure (no infrastructure metadata)
- Same event type works with different ID schemes
- Command handlers focus on behavior, not identity
- IDs travel in the envelope alongside events

**Trade-off**: You can't access the ID inside `Handle<C>`. If needed, include relevant IDs in the command.

```rust,ignore
// ID is infrastructure
repository.execute_command::<Account, Deposit>(&account_id, &command, &metadata)?;

// Event doesn't contain ID
struct FundsDeposited { amount: i64 }  // No account_id field
```

## No Built-In Command Bus

**Decision**: The crate provides `Repository::execute_command()` but no command routing infrastructure.

**Why**:
- Command buses vary widely (sync, async, distributed, in-process)
- Many applications don't need one (web handlers call repository directly)
- Easy to add on top: trait object dispatch, actor messages, etc.
- Keeps the crate focused on event sourcing, not messaging

**What you might build**:
```rust,ignore
// Your command bus (not provided)
trait CommandBus {
    fn dispatch<C: Command>(&self, id: &str, command: C) -> Result<(), Error>;
}
```

## Versioning at the Codec Layer

**Decision**: No explicit "upcaster" concept. Event migration happens in the codec or via serde.

**Why**:
- Simpler mental model—one place for serialization concerns
- Works with any serde-compatible migration library
- Codec can optimize (e.g., cache parsed schemas)
- Avoids framework lock-in for versioning strategy

**How**:
```rust,ignore
// Option 1: serde defaults
#[serde(default)]
pub marketing_consent: bool,

// Option 2: serde-evolve
#[derive(Evolve)]
#[evolve(ancestors(V1, V2))]
pub struct Event { /* ... */ }

// Option 3: Codec-level
impl Codec for VersionedCodec {
    fn deserialize(&self, data: &[u8]) -> Result<E> {
        // Parse, migrate, return current version
    }
}
```

## Projections Decoupled from Aggregates

**Decision**: Projections implement `ApplyProjection<E, M>` for event types, not aggregate types.

**Why**:
- A projection can consume events from many aggregates
- Projections don't need to know about aggregate enums
- Enables cross-aggregate views naturally
- Better separation of read and write concerns

```rust,ignore
// Projection consumes from multiple aggregates
impl ApplyProjection<OrderPlaced, ()> for Dashboard { /* ... */ }
impl ApplyProjection<PaymentReceived, ()> for Dashboard { /* ... */ }
impl ApplyProjection<ShipmentDispatched, ()> for Dashboard { /* ... */ }
```

## Minimal Infrastructure

**Decision**: No outbox, no scheduler, no event streaming, no saga orchestrator.

**Why**:
- Event sourcing is the foundation; infrastructure varies by use case
- Async runtimes differ (tokio, async-std, sync-only)
- Message brokers differ (Kafka, RabbitMQ, NATS, none)
- Database choices affect outbox patterns
- Keeps dependencies minimal
- **may be added in future!!**

**What we do provide**:
- Core traits for aggregates and projections
- Repository for command execution
- In-memory store for testing
- Test framework for aggregate testing

**What you add**:
- Production event store
- Outbox pattern (if needed)
- Process managers / sagas
- Event publishing to message broker
