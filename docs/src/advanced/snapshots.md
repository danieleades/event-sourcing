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
use event_sourcing::{InMemorySnapshotStore, Repository};

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
pub trait SnapshotStore<A: Aggregate> {
    type Error: std::error::Error;
    type Position;

    fn load(
        &self,
        aggregate_kind: &str,
        aggregate_id: &A::Id,
    ) -> Result<Option<Snapshot<A, Self::Position>>, Self::Error>;

    fn offer_snapshot(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &A::Id,
        aggregate: &A,
        position: Self::Position,
        events_since_last_snapshot: usize,
    ) -> Result<(), Self::Error>;
}
```

The `offer_snapshot` method receives `events_since_last_snapshot`, allowing the store to decide based on event count.

## The `Snapshot` Type

```rust,ignore
pub struct Snapshot<A, Pos> {
    pub aggregate: A,
    pub position: Pos,
}
```

The position indicates where to resume loading events.

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
impl SnapshotStore<Account> for PostgresSnapshotStore {
    type Error = sqlx::Error;
    type Position = u64;

    fn load(&self, kind: &str, id: &String)
        -> Result<Option<Snapshot<Account, u64>>, Self::Error>
    {
        // SELECT aggregate_data, position
        // FROM snapshots
        // WHERE aggregate_kind = $1 AND aggregate_id = $2
    }

    fn offer_snapshot(
        &mut self,
        kind: &str,
        id: &String,
        aggregate: &Account,
        position: u64,
        events_since: usize,
    ) -> Result<(), Self::Error> {
        if events_since >= 100 {
            // INSERT INTO snapshots ...
        }
        Ok(())
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
