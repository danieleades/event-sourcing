# Custom Metadata

Metadata carries infrastructure concerns alongside events without polluting domain types. Common uses include correlation IDs, user context, timestamps, and causation tracking.

## Defining Metadata

Create a struct for your metadata:

```rust,ignore
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventMetadata {
    pub correlation_id: String,
    pub causation_id: Option<String>,
    pub user_id: String,
    pub timestamp: DateTime<Utc>,
}
```

## Using Metadata with the Store

Configure your store with the metadata type:

```rust,ignore
use event_sourcing::store::{inmemory, JsonCodec};

let store: inmemory::Store<String, JsonCodec, EventMetadata> =
    inmemory::Store::new(JsonCodec);
```

## Passing Metadata to Commands

Provide metadata when executing commands:

```rust,ignore
let metadata = EventMetadata {
    correlation_id: Uuid::new_v4().to_string(),
    causation_id: None,
    user_id: current_user.id.clone(),
    timestamp: Utc::now(),
};

repository.execute_command::<Account, Deposit>(
    &account_id,
    &Deposit { amount: 100 },
    &metadata,
)
.await?;
```

Each event produced by the command receives this metadata.

## Accessing Metadata in Projections

Projections receive metadata as the third parameter:

```rust,ignore
#[derive(Debug, Default)]
pub struct AuditLog {
    pub entries: Vec<AuditEntry>,
}

impl Projection for AuditLog {
    type Id = String;
    type Metadata = EventMetadata;
}

impl ApplyProjection<FundsDeposited> for AuditLog {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        event: &FundsDeposited,
        meta: &Self::Metadata,
    ) {
        self.entries.push(AuditEntry {
            timestamp: meta.timestamp,
            user: meta.user_id.clone(),
            correlation_id: meta.correlation_id.clone(),
            action: format!("Deposited {} to account {}", event.amount, aggregate_id),
        });
    }
}
```

## Correlation and Causation

Track event relationships for debugging and workflows:

```d2
Request A: "Request A (correlation: abc)" {
  E1: |md
    OrderPlaced
    causation: null
  |
  E2: |md
    InventoryReserved
    causation: OrderPlaced
  |
  E3: |md
    PaymentProcessed
    causation: OrderPlaced
  |

  E1 -> E2
  E1 -> E3
}
```

- **Correlation ID**: Groups all events from a single user request
- **Causation ID**: Points to the event that triggered this one

```rust,ignore
// When handling a saga or process manager
let follow_up_metadata = EventMetadata {
    correlation_id: original_meta.correlation_id.clone(),
    causation_id: Some(original_event_id.to_string()),
    user_id: "system".to_string(),
    timestamp: Utc::now(),
};
```

## Unit Metadata

If you don't need metadata, use `()`:

```rust,ignore
let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);

repository
    .execute_command::<Account, Deposit>(&id, &cmd, &())
    .await?;
```

Projections with `type Metadata = ()` ignore the metadata parameter.

## Metadata vs Event Data

| Put in Metadata | Put in Event |
|-----------------|--------------|
| Who did it (user ID) | What happened (domain facts) |
| When (timestamp) | Domain-relevant times (due date) |
| Request tracing (correlation) | Business identifiers |
| Infrastructure context | Domain context |

Events should be understandable without metadata. Metadata enhances observability.

## Example: Multi-Tenant Metadata

```rust,ignore
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TenantMetadata {
    pub tenant_id: String,
    pub user_id: String,
    pub request_id: String,
}

impl Projection for TenantDashboard {
    type Id = String;
    type Metadata = TenantMetadata;
}

impl ApplyProjection<OrderPlaced> for TenantDashboard {
    fn apply_projection(
        &mut self,
        _id: &Self::Id,
        event: &OrderPlaced,
        meta: &Self::Metadata,
    ) {
        // Only count orders for our tenant
        if meta.tenant_id == self.tenant_id {
            self.order_count += 1;
            self.total_revenue += event.total;
        }
    }
}
```

## Next

[Custom Stores](custom-stores.md) — Implementing your own persistence layer
