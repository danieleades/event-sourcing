//! Application service orchestration.
//!
//! `Repository` coordinates loading aggregates, invoking command handlers, and
//! appending resulting events to the store. Errors are wrapped in `CommandError`
//! for a single call-site surface.
//!
//! ## Snapshotting
//!
//! Repositories can optionally use snapshots to optimize aggregate loading.
//! Use the builder pattern to add a snapshot store:
//!
//! ```ignore
//! let repo = Repository::new(event_store)
//!     .with_snapshots(InMemorySnapshotStore::every(100));
//! ```
//!
//! ## Optimistic Concurrency
//!
//! By default, repositories use last-writer-wins semantics. To enable
//! optimistic concurrency control (version checking on writes):
//!
//! ```ignore
//! let repo = Repository::new(event_store)
//!     .with_optimistic_concurrency();
//! ```

use std::marker::PhantomData;

use thiserror::Error;

use crate::{
    ProjectionEvent, SerializableEvent,
    aggregate::{Aggregate, AggregateBuilder, Handle},
    codec::Codec,
    concurrency::{ConcurrencyConflict, ConcurrencyStrategy, Optimistic, Unchecked},
    projection::{Projection, ProjectionBuilder, ProjectionError},
    snapshot::{NoSnapshots, Snapshot, SnapshotStore},
    store::{AppendError, EventFilter, EventStore},
};

/// Error type for unchecked command execution (no concurrency variant).
#[derive(Debug, Error)]
pub enum CommandError<AggregateError, StoreError, CodecError, SnapshotError>
where
    StoreError: std::error::Error + 'static,
    CodecError: std::error::Error + 'static,
    SnapshotError: std::error::Error + 'static,
{
    #[error("aggregate rejected command: {0}")]
    Aggregate(AggregateError),
    #[error("failed to rebuild aggregate state: {0}")]
    Projection(#[source] ProjectionError<StoreError, CodecError>),
    #[error("failed to encode events: {0}")]
    Codec(#[source] CodecError),
    #[error("failed to persist events: {0}")]
    Store(#[source] StoreError),
    #[error("snapshot operation failed: {0}")]
    Snapshot(#[source] SnapshotError),
}

/// Error type for optimistic command execution (includes concurrency).
#[derive(Debug, Error)]
pub enum OptimisticCommandError<AggregateError, Position, StoreError, CodecError, SnapshotError>
where
    Position: std::fmt::Debug,
    StoreError: std::error::Error + 'static,
    CodecError: std::error::Error + 'static,
    SnapshotError: std::error::Error + 'static,
{
    #[error("aggregate rejected command: {0}")]
    Aggregate(AggregateError),
    #[error(transparent)]
    Concurrency(ConcurrencyConflict<Position>),
    #[error("failed to rebuild aggregate state: {0}")]
    Projection(#[source] ProjectionError<StoreError, CodecError>),
    #[error("failed to encode events: {0}")]
    Codec(#[source] CodecError),
    #[error("failed to persist events: {0}")]
    Store(#[source] StoreError),
    #[error("snapshot operation failed: {0}")]
    Snapshot(#[source] SnapshotError),
}

/// Result type alias for unchecked command execution.
type UncheckedCommandResult<A, S, SS> = Result<
    (),
    CommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

/// Result type alias for optimistic command execution.
type OptimisticCommandResult<A, S, SS> = Result<
    (),
    OptimisticCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Position,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

/// Type alias for projection errors using the store's error types.
type LoadError<S> =
    ProjectionError<<S as EventStore>::Error, <<S as EventStore>::Codec as Codec>::Error>;

/// Result of loading an aggregate with version tracking.
///
/// Contains all the information needed for command execution:
/// - The aggregate state
/// - The version for concurrency checking (position of last event, or snapshot position if no events after)
struct LoadedAggregate<A, Pos> {
    aggregate: A,
    /// Position for optimistic concurrency checking.
    /// This is the position of the last event applied, or the snapshot position if no events followed.
    /// `None` only when no snapshot and no events exist (truly new aggregate).
    version: Option<Pos>,
    _pos: std::marker::PhantomData<Pos>,
}

/// Coordinates loading aggregates and persisting the resulting events.
///
/// The repository orchestrates the command execution flow:
/// 1. Load aggregate from events (optionally using a snapshot)
/// 2. Handle command to produce new events
/// 3. Persist new events
/// 4. Optionally create a new snapshot
///
/// # Type Parameters
///
/// - `S`: Event store implementation
/// - `SS`: Snapshot store implementation
/// - `C`: Concurrency strategy (defaults to [`Unchecked`])
///
/// The `EventStore::Id` and `SnapshotStore::Id` types must match, as must the position types.
pub struct Repository<S, SS, C = Unchecked>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    C: ConcurrencyStrategy,
{
    pub(crate) store: S,
    snapshots: SS,
    _concurrency: PhantomData<C>,
}

impl<S> Repository<S, NoSnapshots<S::Id, S::Position>, Unchecked>
where
    S: EventStore,
{
    /// Create a new repository without snapshot support.
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self {
            store,
            snapshots: NoSnapshots::new(),
            _concurrency: PhantomData,
        }
    }
}

impl<S, SS, C> Repository<S, SS, C>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    C: ConcurrencyStrategy,
{
    /// Add snapshot support to this repository.
    ///
    /// The snapshot store determines both storage and policy for when to create
    /// snapshots. Use implementations like [`InMemorySnapshotStore::every(100)`]
    /// to snapshot after every 100 events.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let repo = Repository::new(InMemoryEventStore::new(JsonCodec))
    ///     .with_snapshots(InMemorySnapshotStore::every(100));
    /// ```
    #[must_use]
    pub fn with_snapshots<NewSS: SnapshotStore<Id = S::Id, Position = S::Position>>(
        self,
        snapshots: NewSS,
    ) -> Repository<S, NewSS, C> {
        Repository {
            store: self.store,
            snapshots,
            _concurrency: PhantomData,
        }
    }

    #[must_use]
    pub const fn event_store(&self) -> &S {
        &self.store
    }

    pub const fn event_store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Access the snapshot store.
    #[must_use]
    pub const fn snapshot_store(&self) -> &SS {
        &self.snapshots
    }

    /// Create a projection builder for flexible query-side loading.
    ///
    /// Use this to build read models by specifying which events to apply
    /// and which streams to load from.
    pub fn build_projection<P>(&self) -> ProjectionBuilder<'_, S, SS, C, P>
    where
        P: Projection,
    {
        ProjectionBuilder::new(self)
    }

    /// Create an aggregate builder for loading command-side entities manually.
    ///
    /// The derive macros generate the event kind list for you, so most callers can rely on
    /// [`execute_command`](Self::execute_command) instead. The builder is useful for custom
    /// loading patterns (snapshots, partial history, etc.).
    pub const fn aggregate_builder<A>(&self) -> AggregateBuilder<'_, S, SS, C, A>
    where
        A: Aggregate<Id = S::Id>,
    {
        AggregateBuilder::new(self)
    }

    /// Load an aggregate and return both the aggregate and its version.
    ///
    /// This is an internal helper used by `execute_command` implementations.
    fn load_aggregate<A>(&self, id: &S::Id) -> Result<LoadedAggregate<A, S::Position>, LoadError<S>>
    where
        A: Aggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
    {
        let codec = self.store.codec();

        // Try to load snapshot (ignore errors from the store - fall back to full replay)
        let snapshot_result = self.snapshots.load(A::KIND, id).ok().flatten();

        // Restore aggregate from snapshot or start fresh
        let (mut aggregate, snapshot_position) = match snapshot_result {
            Some(snapshot) => {
                // Deserialize snapshot using the store's codec
                // Return an error if deserialization fails - this indicates data corruption
                let restored: A = codec
                    .deserialize(&snapshot.data)
                    .map_err(ProjectionError::SnapshotDeserialize)?;
                (restored, Some(snapshot.position))
            }
            None => (A::default(), None),
        };

        // Build filters for events after snapshot position
        let filters: Vec<EventFilter<S::Id, S::Position>> = A::Event::EVENT_KINDS
            .iter()
            .map(|kind| {
                let mut filter = EventFilter::for_aggregate(*kind, A::KIND, id.clone());
                if let Some(pos) = snapshot_position {
                    filter = filter.after(pos);
                }
                filter
            })
            .collect();

        // Load and replay events
        let events = self
            .store
            .load_events(&filters)
            .map_err(ProjectionError::Store)?;

        let mut last_event_position: Option<S::Position> = None;

        for stored in &events {
            let event = A::Event::from_stored(&stored.kind, &stored.data, codec)
                .map_err(ProjectionError::Codec)?;
            aggregate.apply(&event);
            last_event_position = Some(stored.position);
        }

        // Version for concurrency checking:
        // - If we have events after snapshot, use the last event position
        // - If we have a snapshot but no events after, use the snapshot position
        // - If neither, this is a new aggregate (version = None)
        let version = last_event_position.or(snapshot_position);

        Ok(LoadedAggregate {
            aggregate,
            version,
            _pos: std::marker::PhantomData,
        })
    }
}

impl<S, SS> Repository<S, SS, Unchecked>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
    /// Enable optimistic concurrency control for this repository.
    ///
    /// With optimistic concurrency enabled, the repository tracks the stream
    /// version when loading aggregates and verifies it hasn't changed before
    /// appending new events. If the version changed, `execute_command` returns
    /// an [`OptimisticCommandError::Concurrency`] error.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let repo = Repository::new(InMemoryEventStore::new(JsonCodec))
    ///     .with_optimistic_concurrency();
    ///
    /// match repo.execute_command::<Account, Deposit>(&id, &cmd, &meta) {
    ///     Ok(()) => println!("Success"),
    ///     Err(OptimisticCommandError::Concurrency(c)) => {
    ///         println!("Conflict detected, retrying...");
    ///     }
    ///     Err(e) => return Err(e),
    /// }
    /// ```
    #[must_use]
    pub fn with_optimistic_concurrency(self) -> Repository<S, SS, Optimistic> {
        Repository {
            store: self.store,
            snapshots: self.snapshots,
            _concurrency: PhantomData,
        }
    }

    /// Execute a command on an aggregate instance.
    ///
    /// The aggregate is loaded from the event store (optionally using a snapshot),
    /// then the command is handled through the `Handle<C>` trait, and resulting
    /// events are persisted. If a snapshot store is configured, a new snapshot
    /// may be created based on the store's policy.
    ///
    /// This version uses last-writer-wins semantics with no version checking.
    ///
    /// # Type Parameters
    ///
    /// - `A`: The aggregate type
    /// - `Cmd`: The command type (must implement `Handle<Cmd> for A`)
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if:
    /// - The aggregate cannot be loaded (including snapshot deserialization failures)
    /// - The aggregate rejects the command
    /// - Events cannot be serialized
    /// - The store fails to persist events
    /// - The snapshot store fails to save the snapshot
    ///
    /// # Panics
    ///
    /// Panics if `stream_version` returns `None` after successfully appending events.
    /// This indicates a bug in the event store implementation.
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> UncheckedCommandResult<A, S, SS>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent + Clone,
        S::Metadata: Clone,
    {
        // Load aggregate (version not used in unchecked mode)
        let LoadedAggregate { aggregate, .. } = self
            .load_aggregate::<A>(id)
            .map_err(CommandError::Projection)?;

        // Handle the command
        let new_events =
            Handle::<Cmd>::handle(&aggregate, command).map_err(CommandError::Aggregate)?;

        let events_count = new_events.len();
        if events_count == 0 {
            return Ok(());
        }

        // Begin transaction and append events (no version checking)
        let mut tx = self.store.begin::<Unchecked>(A::KIND, id.clone(), None);

        for event in &new_events {
            tx.append(event.clone(), metadata.clone())
                .map_err(CommandError::Codec)?;
        }

        // Commit transaction
        tx.commit().map_err(CommandError::Store)?;

        // Get the new stream position after commit for snapshot
        let new_position = self
            .store
            .stream_version(A::KIND, id)
            .map_err(CommandError::Store)?
            .expect("stream should have events after append");

        // Apply new events to aggregate for potential snapshotting
        let mut aggregate = aggregate;
        for event in new_events {
            aggregate.apply(&event);
        }

        // Offer snapshot to the snapshot store using the store's codec
        let codec = self.store.codec();
        let snapshot = Snapshot {
            position: new_position,
            data: codec.serialize(&aggregate).map_err(CommandError::Codec)?,
        };

        self.snapshots
            .offer_snapshot(A::KIND, id, snapshot, events_count as u64)
            .map_err(CommandError::Snapshot)?;

        Ok(())
    }
}

impl<S, SS> Repository<S, SS, Optimistic>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
    /// Execute a command on an aggregate instance with optimistic concurrency control.
    ///
    /// The aggregate is loaded from the event store (optionally using a snapshot),
    /// then the command is handled through the `Handle<C>` trait, and resulting
    /// events are persisted with version checking.
    ///
    /// If another writer has appended events to the stream since we loaded the
    /// aggregate, this method returns [`OptimisticCommandError::Concurrency`].
    ///
    /// # Type Parameters
    ///
    /// - `A`: The aggregate type
    /// - `Cmd`: The command type (must implement `Handle<Cmd> for A`)
    ///
    /// # Errors
    ///
    /// Returns [`OptimisticCommandError`] if:
    /// - The aggregate cannot be loaded
    /// - The aggregate rejects the command
    /// - Events cannot be serialized
    /// - Another writer modified the stream (concurrency conflict)
    /// - The store fails to persist events
    /// - The snapshot store fails
    ///
    /// # Panics
    ///
    /// Panics if `stream_version` returns `None` after successfully appending events.
    /// This indicates a bug in the event store implementation.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Retry loop for handling conflicts
    /// loop {
    ///     match repo.execute_command::<Account, Deposit>(&id, &cmd, &meta) {
    ///         Ok(()) => break,
    ///         Err(OptimisticCommandError::Concurrency(_)) => continue,
    ///         Err(e) => return Err(e),
    ///     }
    /// }
    /// ```
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> OptimisticCommandResult<A, S, SS>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent + Clone,
        S::Metadata: Clone,
    {
        // Load aggregate and track version
        let LoadedAggregate {
            aggregate, version, ..
        } = self
            .load_aggregate::<A>(id)
            .map_err(OptimisticCommandError::Projection)?;

        // Handle the command
        let new_events = Handle::<Cmd>::handle(&aggregate, command)
            .map_err(OptimisticCommandError::Aggregate)?;

        let events_count = new_events.len();
        if events_count == 0 {
            return Ok(());
        }

        // Begin transaction with expected version for optimistic concurrency
        // version=None means "expect new aggregate" (will be checked by append_new)
        let mut tx = self.store.begin::<Optimistic>(A::KIND, id.clone(), version);

        for event in &new_events {
            tx.append(event.clone(), metadata.clone())
                .map_err(OptimisticCommandError::Codec)?;
        }

        // Commit transaction with version checking
        tx.commit().map_err(|e| match e {
            AppendError::Conflict(c) => OptimisticCommandError::Concurrency(c),
            AppendError::Store(s) => OptimisticCommandError::Store(s),
        })?;

        // Get the new stream position after commit for snapshot
        let new_position = self
            .store
            .stream_version(A::KIND, id)
            .map_err(OptimisticCommandError::Store)?
            .expect("stream should have events after append");

        // Apply new events to aggregate for potential snapshotting
        let mut aggregate = aggregate;
        for event in new_events {
            aggregate.apply(&event);
        }

        // Offer snapshot to the snapshot store using the store's codec
        let codec = self.store.codec();
        let snapshot = Snapshot {
            position: new_position,
            data: codec
                .serialize(&aggregate)
                .map_err(OptimisticCommandError::Codec)?,
        };

        self.snapshots
            .offer_snapshot(A::KIND, id, snapshot, events_count as u64)
            .map_err(OptimisticCommandError::Snapshot)?;

        Ok(())
    }
}
