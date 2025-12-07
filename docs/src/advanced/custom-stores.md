# Custom Stores

The `InMemoryEventStore` is useful for testing, but production systems need durable storage. This guide walks through implementing `EventStore` for your database.

## The EventStore Trait

```rust,ignore
pub trait EventStore {
    type Position;
    type Error: std::error::Error;
    type Codec: Codec;
    type Metadata;

    fn begin(&mut self, aggregate_kind: &str, aggregate_id: &str)
        -> Transaction<'_, Self>;

    fn load_events(&self, filters: &[EventFilter<Self::Position>])
        -> Result<Vec<StoredEvent<Self::Position, Self::Metadata>>, Self::Error>;
}
```

## Design Decisions

### Position Type

Choose based on your ordering needs:

| Position Type | Use Case |
|---------------|----------|
| `()` | Unordered, append-only log |
| `u64` | Global sequence number |
| `(i64, i32)` | Timestamp + sequence for distributed systems |
| `Uuid` | Event IDs for deduplication |

### Storage Schema

A typical SQL schema:

```sql
CREATE TABLE events (
    position BIGSERIAL PRIMARY KEY,
    aggregate_kind TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    data BYTEA NOT NULL,
    metadata JSONB NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

CREATE INDEX idx_events_aggregate
    ON events (aggregate_kind, aggregate_id, position);

CREATE INDEX idx_events_kind
    ON events (event_kind, position);
```

## Implementation Skeleton

```rust,ignore
use event_sourcing::{
    Codec, EventFilter, EventStore, JsonCodec,
    PersistableEvent, StoredEvent, Transaction,
};

pub struct PostgresEventStore {
    pool: sqlx::PgPool,
    codec: JsonCodec,
}

impl EventStore for PostgresEventStore {
    type Position = i64;
    type Error = sqlx::Error;
    type Codec = JsonCodec;
    type Metadata = serde_json::Value;

    fn begin(&mut self, aggregate_kind: &str, aggregate_id: &str)
        -> Transaction<'_, Self>
    {
        Transaction::new(self, aggregate_kind, aggregate_id)
    }

    fn load_events(&self, filters: &[EventFilter<Self::Position>])
        -> Result<Vec<StoredEvent<Self::Position, Self::Metadata>>, Self::Error>
    {
        // Build query from filters
        // Execute and map rows to StoredEvent
        todo!()
    }
}
```

## Implementing Transactions

The `Transaction` type manages event batching:

```rust,ignore
impl PostgresEventStore {
    pub async fn append_batch(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &str,
        events: Vec<PersistableEvent<serde_json::Value>>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        for event in events {
            sqlx::query(
                "INSERT INTO events (aggregate_kind, aggregate_id, event_kind, data, metadata)
                 VALUES ($1, $2, $3, $4, $5)"
            )
            .bind(aggregate_kind)
            .bind(aggregate_id)
            .bind(&event.kind)
            .bind(&event.data)
            .bind(&event.metadata)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }
}
```

## Loading Events

Handle multiple filters efficiently:

```rust,ignore
fn load_events(&self, filters: &[EventFilter<i64>])
    -> Result<Vec<StoredEvent<i64, serde_json::Value>>, sqlx::Error>
{
    // Deduplicate overlapping filters
    // Build WHERE clause:
    //   (event_kind = 'x' AND position > N)
    //   OR (event_kind = 'y' AND aggregate_kind = 'a' AND aggregate_id = 'b')
    // ORDER BY position ASC
    // Map rows to StoredEvent
    todo!()
}
```

## Optimistic Concurrency

For systems requiring strict ordering, add version checking:

```sql
ALTER TABLE events ADD COLUMN stream_version INT NOT NULL;

CREATE UNIQUE INDEX idx_events_stream_version
    ON events (aggregate_kind, aggregate_id, stream_version);
```

```rust,ignore
// In append_batch:
// 1. Get current max version for stream
// 2. Insert with version + 1
// 3. Handle unique constraint violation as concurrency conflict
```

## Event Stores for Different Databases

### DynamoDB

```text
Table: events
  PK: {aggregate_kind}#{aggregate_id}
  SK: {position:012d}  (zero-padded for sorting)
  GSI: event_kind-position-index
```

### MongoDB

```javascript
{
  _id: ObjectId,
  aggregateKind: "account",
  aggregateId: "ACC-001",
  eventKind: "account.deposited",
  position: NumberLong(1234),
  data: BinData(...),
  metadata: { ... }
}
```

### S3 (Append-Only Log)

```text
s3://bucket/events/{aggregate_kind}/{aggregate_id}/{position}.json
s3://bucket/events-by-kind/{event_kind}/{position}.json  (symlinks/copies)
```

## Testing Your Store

Use the same test patterns as `InMemoryEventStore`:

```rust,ignore
#[tokio::test]
async fn test_append_and_load() {
    let store = PostgresEventStore::new(test_pool()).await;

    // Append events
    let mut tx = store.begin("account", "ACC-001");
    tx.append(event, metadata)?;
    tx.commit()?;

    // Load and verify
    let events = store.load_events(&[
        EventFilter::for_aggregate("account.deposited", "account", "ACC-001")
    ])?;

    assert_eq!(events.len(), 1);
}
```

## Next

[Test Framework](../testing/test-framework.md) — Testing aggregates in isolation
