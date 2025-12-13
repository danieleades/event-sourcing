# Projections

Projections are read models built by replaying events. They're optimized for queries rather than consistency. A single event stream can feed many projections, each structuring data differently.

## The `Projection` Trait

```rust,ignore
pub trait Projection: Default + Sized {
    /// Metadata type this projection expects
    type Metadata;
}
```

Projections must be `Default` because they start empty and build up through event replay.

## The `ApplyProjection<E, M>` Trait

```rust,ignore
pub trait ApplyProjection<E, M> {
    fn apply_projection(&mut self, aggregate_id: &str, event: &E, metadata: &M);
}
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
    type Metadata = ();
}

impl ApplyProjection<FundsDeposited, ()> for AccountSummary {
    fn apply_projection(&mut self, id: &str, event: &FundsDeposited, _: &()) {
        *self.accounts.entry(id.to_string()).or_default() += event.amount;
    }
}

impl ApplyProjection<FundsWithdrawn, ()> for AccountSummary {
    fn apply_projection(&mut self, id: &str, event: &FundsWithdrawn, _: &()) {
        *self.accounts.entry(id.to_string()).or_default() -= event.amount;
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
    .load()?;
```

## Multi-Aggregate Projections

Projections can consume events from multiple aggregate types:

```d2
direction: right

Events: {
  ProductCreated
  ProductRenamed
  SaleRecorded
}

Projection: {
  Inventory Report
}

Events.ProductCreated -> Projection.Inventory Report
Events.ProductRenamed -> Projection.Inventory Report
Events.SaleRecorded -> Projection.Inventory Report
```

```rust,ignore
#[derive(Debug, Default)]
pub struct InventoryReport {
    pub products: HashMap<String, ProductInfo>,
    pub total_sales: i64,
}

impl ApplyProjection<ProductCreated, ()> for InventoryReport {
    fn apply_projection(&mut self, id: &str, event: &ProductCreated, _: &()) {
        self.products.insert(id.to_string(), ProductInfo {
            name: event.name.clone(),
            sales_count: 0,
        });
    }
}

impl ApplyProjection<SaleRecorded, ()> for InventoryReport {
    fn apply_projection(&mut self, id: &str, event: &SaleRecorded, _: &()) {
        if let Some(product) = self.products.get_mut(&event.product_id) {
            product.sales_count += event.quantity;
        }
        self.total_sales += event.total;
    }
}
```

Build by registering events from different aggregates:

```rust,ignore
let report = repository
    .build_projection::<InventoryReport>()
    .event::<ProductCreated>()   // From Product aggregate
    .event::<ProductRenamed>()   // From Product aggregate
    .event::<SaleRecorded>()     // From Sale aggregate
    .load()?;
```

## Filtering by Aggregate

Use `.events_for()` to load all events for a specific aggregate instance:

```rust,ignore
let account_history = repository
    .build_projection::<TransactionHistory>()
    .events_for::<Account>(&account_id)
    .load()?;
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
    type Metadata = EventMetadata;
}

impl ApplyProjection<FundsDeposited, EventMetadata> for AuditLog {
    fn apply_projection(&mut self, id: &str, event: &FundsDeposited, meta: &EventMetadata) {
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
