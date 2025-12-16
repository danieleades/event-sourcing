# Snapshots

Replaying events gets expensive as aggregates accumulate history. Snapshots checkpoint aggregate state, allowing you to skip replaying old events.

## How Snapshots Work

```d2
shape: sequence_diagram

App: Application
Snap: SnapshotStore
Store: EventStore
Agg: Aggregate

App -> Snap: 'load("account", "ACC-001")'
Snap -> App: "Snapshot at position 1000" {style.stroke-dash: 3}
App -> Agg: "Deserialize snapshot"
App."balance = 5000 (from snapshot)"
App -> Store: "load_events(after: 1000)"
Store -> App: "Events 1001-1050" {style.stroke-dash: 3}
App -> Agg: "apply(event) [For each new event]"
App."balance = 5250 (current)"
```

Instead of replaying 1050 events, you load the snapshot and replay only 50.

## Enabling Snapshots

Use `with_snapshots()` when creating the repository:

```rust,ignore
use event_sourcing::{InMemoryEventStore, InMemorySnapshotStore, JsonCodec, Repository};

let event_store = InMemoryEventStore::new(JsonCodec);
let snapshot_store = InMemorySnapshotStore::always();

let mut repository = Repository::new(event_store)
    .with_snapshots(snapshot_store);
```

## Snapshot Policies

`InMemorySnapshotStore` provides three policies:

### Always

Save a snapshot after every command:

```rust,ignore
let snapshots = InMemorySnapshotStore::always();
```

Use for: Aggregates with expensive replay, testing.

### Every N Events

Save after accumulating N events since the last snapshot:

```rust,ignore
let snapshots = InMemorySnapshotStore::every(100);
```

Use for: Production workloads balancing storage vs. replay cost.

### Never

Never save (load-only mode):

```rust,ignore
let snapshots = InMemorySnapshotStore::never();
```

Use for: Read-only replicas, debugging.

## The `SnapshotStore` Trait

```rust,ignore
pub trait SnapshotStore: Send + Sync {
    type Id: Send + Sync + 'static;
    type Position: Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    fn load<'a>(
        &'a self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
    ) -> impl std::future::Future<Output = Result<Option<Snapshot<Self::Position>>, Self::Error>>
           + Send
           + 'a;

    fn offer_snapshot<'a, CE, Create>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        events_since_last_snapshot: u64,
        create_snapshot: Create,
    ) -> impl std::future::Future<
        Output = Result<SnapshotOffer, OfferSnapshotError<Self::Error, CE>>,
    > + Send + 'a
    where
        CE: std::error::Error + Send + Sync + 'static,
        Create: FnOnce() -> Result<Snapshot<Self::Position>, CE> + 'a;
}
```

The repository calls `offer_snapshot` after successfully appending new events. Implementations may
decline without invoking `create_snapshot`, avoiding unnecessary snapshot encoding work.

## The `Snapshot` Type

```rust,ignore
pub struct Snapshot<Pos> {
    pub position: Pos,
    pub data: Vec<u8>,
}
```

The `data` is the serialized aggregate state encoded using the repository's event codec.

## When to Snapshot

| Aggregate Type | Recommendation |
|---------------|----------------|
| Short-lived (< 100 events) | Skip snapshots |
| Medium (100-1000 events) | Every 100-500 events |
| Long-lived (1000+ events) | Every 100 events |
| High-throughput | Every N events, tuned to your SLA |

## Implementing a Custom Store

For production, implement `SnapshotStore` with your database:

```rust,ignore
impl SnapshotStore for PostgresSnapshotStore {
    type Error = sqlx::Error;
    type Id = String;
    type Position = u64;

    fn load<'a>(
        &'a self,
        kind: &'a str,
        id: &'a String,
    ) -> impl std::future::Future<Output = Result<Option<Snapshot<u64>>, Self::Error>>
           + Send
           + 'a {
        // SELECT aggregate_data, position
        // FROM snapshots
        // WHERE aggregate_kind = $1 AND aggregate_id = $2
        async move { todo!() }
    }

    fn offer_snapshot<'a, CE, Create>(
        &'a mut self,
        kind: &'a str,
        id: &'a String,
        events_since: u64,
        create_snapshot: Create,
    ) -> impl std::future::Future<
        Output = Result<SnapshotOffer, OfferSnapshotError<Self::Error, CE>>,
    > + Send + 'a
    where
        CE: std::error::Error + Send + Sync + 'static,
        Create: FnOnce() -> Result<Snapshot<u64>, CE> + 'a,
    {
        if events_since < 100 {
            return std::future::ready(Ok(SnapshotOffer::Declined));
        }

        let snapshot = match create_snapshot() {
            Ok(snapshot) => snapshot,
            Err(e) => return std::future::ready(Err(OfferSnapshotError::Create(e))),
        };

        // INSERT INTO snapshots (kind, id, position, data) VALUES (...)
        async move { Ok(SnapshotOffer::Stored) }
    }
}
```

## Snapshot Invalidation

Snapshots are tied to your aggregate's serialized form. When you change the struct:

1. **Add fields** — Use `#[serde(default)]` for backwards compatibility
2. **Remove fields** — Old snapshots still deserialize (extra fields ignored)
3. **Rename fields** — Use `#[serde(alias = "old_name")]`
4. **Change types** — Old snapshots become invalid; delete them

For major changes, delete old snapshots and let them rebuild from events.

## Next

[Event Versioning](event-versioning.md) — Evolving event schemas over time
