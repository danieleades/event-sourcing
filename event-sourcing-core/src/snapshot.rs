//! Snapshot support for optimized aggregate loading.
//!
//! Snapshots persist aggregate state at a point in time, reducing the number of
//! events that need to be replayed when loading an aggregate. This module provides:
//!
//! - [`Snapshot`] - Point-in-time aggregate state
//! - [`SnapshotStore`] - Trait for snapshot persistence with policy
//! - [`NoSnapshots`] - No-op implementation (use `Repository` instead if you want no snapshots)
//! - [`InMemorySnapshotStore`] - Reference implementation with configurable policy

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;

use crate::store::StreamKey;

/// Point-in-time snapshot of aggregate state.
///
/// The `position` field indicates the event stream position when this snapshot
/// was taken. When loading an aggregate, only events after this position need
/// to be replayed.
///
/// Schema evolution is handled at the serialization layer (e.g., via `serde_evolve`),
/// so no version field is needed here.
///
/// # Type Parameters
///
/// - `Pos`: The position type used by the event store (e.g., `u64`, `i64`, etc.)
#[derive(Clone, Debug)]
pub struct Snapshot<Pos> {
    /// Event position when this snapshot was taken.
    pub position: Pos,
    /// Serialized aggregate state.
    pub data: Vec<u8>,
}

/// Trait for snapshot persistence with built-in policy.
///
/// Implementations decide both *how* to store snapshots and *when* to store them.
/// The repository calls [`should_snapshot`](SnapshotStore::should_snapshot) after
/// each successful command execution to decide whether to serialize and persist
/// a new snapshot.
///
/// # Example Implementations
///
/// - Always save: useful for aggregates with expensive replay
/// - Every N events: balance between storage and replay cost
/// - Never save: read-only replicas that only load snapshots created elsewhere
pub trait SnapshotStore: Send + Sync {
    /// Aggregate identifier type.
    ///
    /// Must match the `EventStore::Id` type used in the same repository.
    type Id: Send + Sync + 'static;

    /// Position type for tracking snapshot positions.
    ///
    /// Must match the `EventStore::Position` type used in the same repository.
    type Position: Send + Sync + 'static;

    /// Error type for snapshot operations.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Load the most recent snapshot for an aggregate.
    ///
    /// Returns `Ok(None)` if no snapshot exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying storage fails.
    fn load<'a>(
        &'a self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
    ) -> impl Future<Output = Result<Option<Snapshot<Self::Position>>, Self::Error>> + Send + 'a;

    /// Whether a snapshot should be taken.
    ///
    /// The repository calls this before serializing aggregate state, so snapshot
    /// stores can avoid unnecessary serialization cost when a policy declines.
    #[must_use]
    fn should_snapshot(
        &self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
        events_since_last_snapshot: u64,
    ) -> bool;

    /// Persist a snapshot.
    ///
    /// Called after [`should_snapshot`](SnapshotStore::should_snapshot) returned `true`.
    ///
    /// # Arguments
    ///
    /// * `aggregate_kind` - The aggregate type identifier
    /// * `aggregate_id` - The aggregate instance identifier
    /// * `snapshot` - The snapshot to potentially store
    /// * `events_since_last_snapshot` - Number of events replayed since the last snapshot
    ///
    /// # Errors
    ///
    /// Returns an error if persistence fails.
    fn offer_snapshot<'a>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        snapshot: Snapshot<Self::Position>,
        events_since_last_snapshot: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;
}

/// No-op snapshot store for backwards compatibility.
///
/// This implementation:
/// - Always returns `None` from `load()`
/// - Silently discards all offered snapshots
///
/// Use this as the default when snapshots are not needed.
///
/// Generic over `Id` and `Pos` to match the `EventStore` types.
#[derive(Clone, Debug, Default)]
pub struct NoSnapshots<Id, Pos>(std::marker::PhantomData<(Id, Pos)>);

impl<Id, Pos> NoSnapshots<Id, Pos> {
    /// Create a new no-op snapshot store.
    #[must_use]
    pub const fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<Id, Pos> SnapshotStore for NoSnapshots<Id, Pos>
where
    Id: Send + Sync + 'static,
    Pos: Send + Sync + 'static,
{
    type Id = Id;
    type Position = Pos;
    type Error = Infallible;

    fn load<'a>(
        &'a self,
        _aggregate_kind: &'a str,
        _aggregate_id: &'a Self::Id,
    ) -> impl Future<Output = Result<Option<Snapshot<Pos>>, Self::Error>> + Send + 'a {
        std::future::ready(Ok(None))
    }

    fn offer_snapshot<'a>(
        &'a mut self,
        _aggregate_kind: &'a str,
        _aggregate_id: &'a Self::Id,
        _snapshot: Snapshot<Pos>,
        _events_since_last_snapshot: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a {
        std::future::ready(Ok(()))
    }

    fn should_snapshot(
        &self,
        _aggregate_kind: &str,
        _aggregate_id: &Self::Id,
        _events_since_last_snapshot: u64,
    ) -> bool {
        false
    }
}

/// Snapshot creation policy.
///
/// # Choosing a Policy
///
/// The right policy depends on your aggregate's characteristics:
///
/// | Policy | Best For | Trade-off |
/// |--------|----------|-----------|
/// | `Always` | Expensive replay, low write volume | Storage cost per command |
/// | `EveryNEvents(n)` | Most use cases | Balanced storage vs replay |
/// | `Never` | Read replicas, external snapshot management | Full replay every load |
///
/// ## `Always`
///
/// Creates a snapshot after every command. Best for aggregates where:
/// - Event replay is computationally expensive
/// - Aggregates have many events (100+)
/// - Read latency is more important than write overhead
/// - Write volume is relatively low
///
/// ## `EveryNEvents(n)`
///
/// Creates a snapshot every N events. Recommended for most use cases.
/// - Start with `n = 50-100` and tune based on profiling
/// - Balances storage cost against replay time
/// - Works well for aggregates with moderate event counts
///
/// ## `Never`
///
/// Never creates snapshots. Use when:
/// - Running a read replica that consumes snapshots created elsewhere
/// - Aggregates are short-lived (few events per instance)
/// - Managing snapshots through an external process
/// - Testing without snapshot overhead
#[derive(Clone, Debug)]
enum SnapshotPolicy {
    /// Create a snapshot after every command.
    Always,
    /// Create a snapshot every N events.
    EveryNEvents(u64),
    /// Never create snapshots (load-only mode).
    Never,
}

impl SnapshotPolicy {
    const fn should_snapshot(&self, events_since: u64) -> bool {
        match self {
            Self::Always => true,
            Self::EveryNEvents(threshold) => events_since >= *threshold,
            Self::Never => false,
        }
    }
}

/// In-memory snapshot store with configurable policy.
///
/// This is a reference implementation suitable for testing and development.
/// Production systems should implement [`SnapshotStore`] with durable storage.
///
/// Generic over `Id` and `Pos` to match the `EventStore` types.
///
/// # Example
///
/// ```ignore
/// use event_sourcing::{Repository, InMemoryEventStore, InMemorySnapshotStore, JsonCodec};
///
/// let repo = Repository::new(InMemoryEventStore::new(JsonCodec))
///     .with_snapshots(InMemorySnapshotStore::every(100));
/// ```
#[derive(Clone, Debug)]
pub struct InMemorySnapshotStore<Id, Pos> {
    snapshots: HashMap<StreamKey<Id>, Snapshot<Pos>>,
    policy: SnapshotPolicy,
}

impl<Id, Pos> InMemorySnapshotStore<Id, Pos> {
    /// Create a snapshot store that saves after every command.
    ///
    /// Best for aggregates with expensive replay or many events.
    /// See the policy guidelines above for choosing an appropriate cadence.
    #[must_use]
    pub fn always() -> Self {
        Self {
            snapshots: HashMap::new(),
            policy: SnapshotPolicy::Always,
        }
    }

    /// Create a snapshot store that saves every N events.
    ///
    /// Recommended for most use cases. Start with `n = 50-100` and tune
    /// based on your aggregate's replay cost.
    /// See the policy guidelines above for choosing a policy.
    #[must_use]
    pub fn every(n: u64) -> Self {
        Self {
            snapshots: HashMap::new(),
            policy: SnapshotPolicy::EveryNEvents(n),
        }
    }

    /// Create a snapshot store that never saves (load-only).
    ///
    /// Use for read replicas, short-lived aggregates, or when managing
    /// snapshots externally. See the policy guidelines above for when this fits.
    #[must_use]
    pub fn never() -> Self {
        Self {
            snapshots: HashMap::new(),
            policy: SnapshotPolicy::Never,
        }
    }
}

impl<Id, Pos> Default for InMemorySnapshotStore<Id, Pos> {
    fn default() -> Self {
        Self::always()
    }
}

impl<Id, Pos> SnapshotStore for InMemorySnapshotStore<Id, Pos>
where
    Id: Clone + Eq + std::hash::Hash + Send + Sync + 'static,
    Pos: Clone + Send + Sync + 'static,
{
    type Id = Id;
    type Position = Pos;
    type Error = Infallible;

    #[tracing::instrument(skip(self, aggregate_id))]
    fn load<'a>(
        &'a self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
    ) -> impl Future<Output = Result<Option<Snapshot<Pos>>, Self::Error>> + Send + 'a {
        let key = StreamKey::new(aggregate_kind, aggregate_id.clone());
        let snapshot = self.snapshots.get(&key).cloned();
        tracing::trace!(found = snapshot.is_some(), "snapshot lookup");
        std::future::ready(Ok(snapshot))
    }

    fn should_snapshot(
        &self,
        _aggregate_kind: &str,
        _aggregate_id: &Self::Id,
        events_since_last_snapshot: u64,
    ) -> bool {
        self.policy.should_snapshot(events_since_last_snapshot)
    }

    #[tracing::instrument(skip(self, aggregate_id, snapshot))]
    fn offer_snapshot<'a>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        snapshot: Snapshot<Pos>,
        events_since_last_snapshot: u64,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a {
        debug_assert!(
            self.policy.should_snapshot(events_since_last_snapshot),
            "offer_snapshot called when policy declined"
        );

        let key = StreamKey::new(aggregate_kind, aggregate_id.clone());
        self.snapshots.insert(key, snapshot);
        tracing::debug!(events_since_last_snapshot, "snapshot saved");
        std::future::ready(Ok(()))
    }
}
