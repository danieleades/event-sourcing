# Projections

Projections are read models built by replaying events. They're optimized for queries rather than consistency. A single event stream can feed many projections, each structuring data differently.

## The `Projection` Trait

```rust,ignore
{{#include ../../../sourcery-core/src/projection.rs:projection_trait}}
```

Projections must be `Default` because they start empty and build up through event replay.

## The `ApplyProjection<E>` Trait

```rust,ignore
{{#include ../../../sourcery-core/src/projection.rs:apply_projection_trait}}
```

Unlike aggregate `Apply`, projections receive:

- `aggregate_id` — Which instance produced this event
- `event` — The domain event
- `metadata` — Store-provided metadata (timestamps, correlation IDs, etc.)

## Basic Example

```rust,ignore
#[derive(Debug, Default)]
pub struct AccountSummary {
    pub accounts: HashMap<String, i64>,
}

impl Projection for AccountSummary {
    type Id = String;
    type Metadata = ();
}

impl ApplyProjection<FundsDeposited> for AccountSummary {
    fn apply_projection(&mut self, id: &Self::Id, event: &FundsDeposited, _: &Self::Metadata) {
        *self.accounts.entry(id.clone()).or_default() += event.amount;
    }
}

impl ApplyProjection<FundsWithdrawn> for AccountSummary {
    fn apply_projection(&mut self, id: &Self::Id, event: &FundsWithdrawn, _: &Self::Metadata) {
        *self.accounts.entry(id.clone()).or_default() -= event.amount;
    }
}
```

## Building Projections

Use `ProjectionBuilder` to specify which events to load:

```rust,ignore
let summary = repository
    .build_projection::<AccountSummary>()
    .event::<FundsDeposited>()     // All deposits across all accounts
    .event::<FundsWithdrawn>()      // All withdrawals across all accounts
    .load()
    .await?;
```

## Multi-Aggregate Projections

Projections can consume events from multiple aggregate types. Register each event type with `.event::<E>()`:

```rust,ignore
let report = repository
    .build_projection::<InventoryReport>()
    .event::<ProductCreated>()   // From Product aggregate
    .event::<SaleRecorded>()     // From Sale aggregate
    .load()
    .await?;
```

Implement `ApplyProjection<E>` for each event type the projection handles.

## Filtering by Aggregate

Use `.events_for()` to load all events for a specific aggregate instance:

```rust,ignore
let account_history = repository
    .build_projection::<TransactionHistory>()
    .events_for::<Account>(&account_id)
    .load()
    .await?;
```

## Using Metadata

Projections can access event metadata for cross-cutting concerns:

```rust,ignore
#[derive(Debug)]
pub struct EventMetadata {
    pub timestamp: DateTime<Utc>,
    pub user_id: String,
}

#[derive(Debug, Default)]
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
}

impl Projection for AuditLog {
    type Id = String;
    type Metadata = EventMetadata;
}

impl ApplyProjection<FundsDeposited> for AuditLog {
    fn apply_projection(&mut self, id: &Self::Id, event: &FundsDeposited, meta: &Self::Metadata) {
        self.entries.push(AuditEntry {
            timestamp: meta.timestamp,
            user: meta.user_id.clone(),
            action: format!("Deposited {} to {}", event.amount, id),
        });
    }
}
```

## Projections vs Aggregates

| Aspect | Aggregate | Projection |
|--------|-----------|------------|
| Purpose | Enforce invariants | Serve queries |
| State source | Own events only | Any events |
| Receives IDs | No (in envelope) | Yes (as parameter) |
| Receives metadata | No | Yes |
| Consistency | Strong | Eventual |
| Mutability | Via commands only | Rebuilt on demand |

## Next

[Stores & Codecs](stores.md) — Event persistence and serialization
