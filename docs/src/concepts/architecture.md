# Architecture

This page shows how the crate's components connect and where your domain code fits in.

## Component Overview

```d2
App: "Your Application" {
  Command
  Aggregate Definition
  Event Definitions
  Projection Definition
}

Crate: "event-sourcing crate" {
  Repository
  SnapshotRepository
  EventStore {shape: cylinder}
  SnapshotStore {shape: cylinder}
  Codec

  Repository -> EventStore
  SnapshotRepository -> SnapshotStore: {style.stroke-dash: 3}
  EventStore -> Codec
}

App.Command -> Crate.Repository
App.Aggregate Definition -> Crate.Repository
App.Event Definitions -> Crate.Repository
App.Projection Definition -> Crate.Repository
```

**You define**: Aggregates, events, commands, projections

**The crate provides**: Repository orchestration, store abstraction, serialization

## The Envelope Pattern

Events travel with metadata, but domain types stay pure:

```d2
direction: right

Envelope: "Event Envelope" {
  Metadata: |md
    aggregate_id, kind,
    correlation_id, timestamp
  |
  Event: |md
    Domain Event
    (pure business data)
  |
}
```

- **Aggregates** receive only the pure event—no IDs or metadata
- **Projections** receive the full envelope—aggregate ID and metadata included
- **Stores** persist the envelope but deserialize only what's needed

This keeps domain logic free of infrastructure concerns.

## Command Execution Flow

```d2
shape: sequence_diagram

App: Application
Repo: SnapshotRepository
Store: EventStore
Agg: Aggregate
Snap: SnapshotStore

App -> Repo: "execute_command(id, cmd, meta)"
Repo -> Snap: "load(id)?"
Snap -> Repo: "Optional snapshot" {style.stroke-dash: 3}
Repo -> Store: "load_events(filters)"
Store -> Repo: "Vec<StoredEvent>" {style.stroke-dash: 3}
Repo -> Agg: "replay events (apply)"
Repo -> Agg: "handle(command)"
Agg -> Repo: "Vec<Event>" {style.stroke-dash: 3}
Repo -> Store: "begin transaction"
Repo -> Store: "append events"
Repo -> Store: "commit"
Repo -> Snap: "offer_snapshot()?"
Repo -> App: "Ok(())" {style.stroke-dash: 3}
```

1. Optionally load a snapshot to skip replaying old events
2. Load remaining events from the store
3. Replay events to rebuild aggregate state
4. Execute the command handler
5. Persist new events atomically
6. Optionally create a new snapshot

## Projection Building Flow

```d2
shape: sequence_diagram

App: Application
Repo: Repository
Store: EventStore
Proj: Projection

App -> Repo: "build_projection()"
App -> Repo: ".event::<E1>().event::<E2>()"
App -> Repo: ".load()"
Repo -> Store: "load_events(filters)"
Store -> Repo: "Vec<StoredEvent>" {style.stroke-dash: 3}
Repo -> Proj: "default()"
Repo -> Proj: "apply_projection(id, event, meta) [For each event]"
Repo -> App: "Projection" {style.stroke-dash: 3}
```

Projections specify which events they care about. The repository loads only those events and replays them into the projection.

## Key Types

| Type | Role |
|------|------|
| `Repository<S>` | Orchestrates aggregates and projections (no snapshots) |
| `SnapshotRepository<S, SS>` | Snapshot-enabled repository orchestration |
| `EventStore` | Trait for event persistence |
| `SnapshotStore` | Trait for aggregate snapshots |
| `Codec` | Trait for serialization/deserialization |
| `Aggregate` | Trait for command-side entities |
| `Projection` | Trait for read-side views |
| `DomainEvent` | Marker trait for event structs |

## Next

[Installation](../getting-started/installation.md) — Add the crate to your project
