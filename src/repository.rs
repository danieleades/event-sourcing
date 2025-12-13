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
//! ## Concurrency Control
//!
//! By default, repositories use optimistic concurrency control - they track the
//! stream version when loading and verify it hasn't changed before appending.
//! If another writer modified the stream, `execute_command` returns an
//! [`OptimisticCommandError::Concurrency`] error.
//!
//! For single-writer scenarios where concurrency checking is unnecessary:
//!
//! ```ignore
//! let repo = Repository::new(event_store)
//!     .without_concurrency_checking();
//! ```

use std::marker::PhantomData;

use thiserror::Error;

use crate::{
    aggregate::{Aggregate, AggregateBuilder, Handle},
    codec::{Codec, ProjectionEvent, SerializableEvent},
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
pub type UncheckedCommandResult<A, S, SS> = Result<
    (),
    CommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

/// Result type alias for optimistic command execution.
pub type OptimisticCommandResult<A, S, SS> = Result<
    (),
    OptimisticCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Position,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

/// Result type alias for retry operations.
///
/// Used with [`Repository::execute_with_retry`] where the success value is the
/// number of attempts taken (1 = succeeded first try, 2 = one retry, etc.).
pub type RetryResult<A, S, SS> = Result<
    usize,
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
/// - The number of events since the last snapshot (for snapshot policy decisions)
struct LoadedAggregate<A, Pos> {
    aggregate: A,
    /// Position for optimistic concurrency checking.
    /// This is the position of the last event applied, or the snapshot position if no events followed.
    /// `None` only when no snapshot and no events exist (truly new aggregate).
    version: Option<Pos>,
    /// Number of events replayed since the last snapshot (or total events if no snapshot).
    /// Used to correctly compute when to create new snapshots.
    events_since_snapshot: u64,
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
/// - `C`: Concurrency strategy (defaults to [`Optimistic`])
///
/// The `EventStore::Id` and `SnapshotStore::Id` types must match, as must the position types.
pub struct Repository<S, SS, C = Optimistic>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    C: ConcurrencyStrategy,
{
    pub(crate) store: S,
    snapshots: SS,
    _concurrency: PhantomData<C>,
}

impl<S> Repository<S, NoSnapshots<S::Id, S::Position>, Optimistic>
where
    S: EventStore,
{
    /// Create a new repository without snapshot support.
    ///
    /// By default, repositories use optimistic concurrency control. This means
    /// the repository tracks the stream version when loading aggregates and
    /// verifies it hasn't changed before appending new events. If the version
    /// changed (another writer appended events), `execute_command` returns
    /// an [`OptimisticCommandError::Concurrency`] error.
    ///
    /// For single-writer scenarios where concurrency checking is unnecessary,
    /// use [`without_concurrency_checking()`](Self::without_concurrency_checking).
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
    /// snapshots. Use implementations like
    /// [`InMemorySnapshotStore::every`](crate::InMemorySnapshotStore::every)
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
    #[tracing::instrument(
        skip(self, id),
        fields(aggregate_kind = A::KIND)
    )]
    fn load_aggregate<A>(&self, id: &S::Id) -> Result<LoadedAggregate<A, S::Position>, LoadError<S>>
    where
        A: Aggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
    {
        let codec = self.store.codec();

        // Try to load snapshot (log errors but fall back to full replay)
        let snapshot_result = match self.snapshots.load(A::KIND, id) {
            Ok(snapshot) => {
                if snapshot.is_some() {
                    tracing::debug!("loaded snapshot");
                }
                snapshot
            }
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to load snapshot, falling back to full replay"
                );
                None
            }
        };

        // Restore aggregate from snapshot or start fresh
        let (mut aggregate, snapshot_position) = if let Some(snapshot) = snapshot_result {
            // Deserialize snapshot using the store's codec
            // Return an error if deserialization fails - this indicates data corruption
            let restored: A = codec
                .deserialize(&snapshot.data)
                .map_err(ProjectionError::SnapshotDeserialize)?;
            tracing::trace!("deserialized aggregate from snapshot");
            (restored, Some(snapshot.position))
        } else {
            tracing::trace!("starting with default aggregate state");
            (A::default(), None)
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

        let events_count = events.len();
        tracing::debug!(events_replayed = events_count, "replaying events");

        let mut last_event_position: Option<S::Position> = None;

        for stored in &events {
            let event = A::Event::from_stored(&stored.kind, &stored.data, codec)
                .map_err(ProjectionError::EventDecode)?;
            aggregate.apply(&event);
            last_event_position = Some(stored.position);
        }

        // Version for concurrency checking:
        // - If we have events after snapshot, use the last event position
        // - If we have a snapshot but no events after, use the snapshot position
        // - If neither, this is a new aggregate (version = None)
        let version = last_event_position.or(snapshot_position);

        tracing::debug!(
            events_replayed = events_count,
            has_version = version.is_some(),
            "aggregate loaded"
        );

        Ok(LoadedAggregate {
            aggregate,
            version,
            events_since_snapshot: events_count as u64,
        })
    }
}

impl<S, SS> Repository<S, SS, Optimistic>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
    /// Disable concurrency checking for this repository.
    ///
    /// With concurrency checking disabled, events are appended without
    /// verifying whether other events were added since loading. This uses
    /// last-writer-wins semantics.
    ///
    /// # When to use
    ///
    /// This is appropriate for:
    /// - Single-writer architectures (actor model, partition-per-writer)
    /// - Testing and prototyping
    /// - Domains where last-writer-wins is semantically correct
    ///
    /// # Example
    ///
    /// ```ignore
    /// let repo = Repository::new(InMemoryEventStore::new(JsonCodec))
    ///     .without_concurrency_checking();
    ///
    /// // Commands will never fail with concurrency errors
    /// repo.execute_command::<Account, Deposit>(&id, &cmd, &meta)?;
    /// ```
    #[must_use]
    pub fn without_concurrency_checking(self) -> Repository<S, SS, Unchecked> {
        Repository {
            store: self.store,
            snapshots: self.snapshots,
            _concurrency: PhantomData,
        }
    }
}

impl<S, SS> Repository<S, SS, Unchecked>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
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
    #[tracing::instrument(
        skip(self, id, command, metadata),
        fields(aggregate_kind = A::KIND)
    )]
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> UncheckedCommandResult<A, S, SS>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        // Load aggregate (version not used in unchecked mode)
        let LoadedAggregate {
            aggregate,
            events_since_snapshot,
            ..
        } = self
            .load_aggregate::<A>(id)
            .map_err(CommandError::Projection)?;

        // Handle the command
        tracing::trace!("handling command");
        let new_events = match Handle::<Cmd>::handle(&aggregate, command) {
            Ok(events) => events,
            Err(e) => {
                tracing::debug!("command rejected by aggregate");
                return Err(CommandError::Aggregate(e));
            }
        };

        let events_count = new_events.len();
        if events_count == 0 {
            tracing::debug!("command produced no events");
            return Ok(());
        }

        tracing::debug!(events_produced = events_count, "command handled");

        // Apply new events to aggregate for potential snapshotting.
        let mut aggregate = aggregate;
        for event in &new_events {
            aggregate.apply(event);
        }

        // Begin transaction and append events (no version checking).
        let mut tx = self.store.begin::<Unchecked>(A::KIND, id.clone(), None);
        for event in new_events {
            tx.append(event, metadata.clone())
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

        // Offer snapshot to the snapshot store using the store's codec
        let codec = self.store.codec();
        let snapshot = Snapshot {
            position: new_position,
            data: codec.serialize(&aggregate).map_err(CommandError::Codec)?,
        };

        // Total events since last snapshot = events replayed + new events
        let total_events_since_snapshot = events_since_snapshot + events_count as u64;
        self.snapshots
            .offer_snapshot(A::KIND, id, snapshot, total_events_since_snapshot)
            .map_err(CommandError::Snapshot)?;

        tracing::info!(events_persisted = events_count, "command executed");
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
    #[tracing::instrument(
        skip(self, id, command, metadata),
        fields(aggregate_kind = A::KIND)
    )]
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> OptimisticCommandResult<A, S, SS>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        // Load aggregate and track version
        let LoadedAggregate {
            aggregate,
            version,
            events_since_snapshot,
        } = self
            .load_aggregate::<A>(id)
            .map_err(OptimisticCommandError::Projection)?;

        // Handle the command
        tracing::trace!("handling command");
        let new_events = match Handle::<Cmd>::handle(&aggregate, command) {
            Ok(events) => events,
            Err(e) => {
                tracing::debug!("command rejected by aggregate");
                return Err(OptimisticCommandError::Aggregate(e));
            }
        };

        let events_count = new_events.len();
        if events_count == 0 {
            tracing::debug!("command produced no events");
            return Ok(());
        }

        tracing::debug!(events_produced = events_count, "command handled");

        // Apply new events to aggregate for potential snapshotting.
        let mut aggregate = aggregate;
        for event in &new_events {
            aggregate.apply(event);
        }

        // Begin transaction with expected version for optimistic concurrency
        // version=None means "expect new aggregate" (will be checked by append_new)
        let mut tx = self.store.begin::<Optimistic>(A::KIND, id.clone(), version);

        for event in new_events {
            tx.append(event, metadata.clone())
                .map_err(OptimisticCommandError::Codec)?;
        }

        // Commit transaction with version checking
        if let Err(e) = tx.commit() {
            match e {
                AppendError::Conflict(c) => {
                    tracing::debug!(
                        expected = ?c.expected,
                        actual = ?c.actual,
                        "concurrency conflict detected"
                    );
                    return Err(OptimisticCommandError::Concurrency(c));
                }
                AppendError::Store(s) => return Err(OptimisticCommandError::Store(s)),
            }
        }

        // Get the new stream position after commit for snapshot
        let new_position = self
            .store
            .stream_version(A::KIND, id)
            .map_err(OptimisticCommandError::Store)?
            .expect("stream should have events after append");

        // Offer snapshot to the snapshot store using the store's codec
        let codec = self.store.codec();
        let snapshot = Snapshot {
            position: new_position,
            data: codec
                .serialize(&aggregate)
                .map_err(OptimisticCommandError::Codec)?,
        };

        // Total events since last snapshot = events replayed + new events
        let total_events_since_snapshot = events_since_snapshot + events_count as u64;
        self.snapshots
            .offer_snapshot(A::KIND, id, snapshot, total_events_since_snapshot)
            .map_err(OptimisticCommandError::Snapshot)?;

        tracing::info!(events_persisted = events_count, "command executed");
        Ok(())
    }

    /// Execute a command with automatic retry on concurrency conflicts.
    ///
    /// When a [`OptimisticCommandError::Concurrency`] error occurs, the command is
    /// retried with fresh aggregate state up to `max_retries` times. Other errors
    /// are returned immediately without retry.
    ///
    /// Returns the number of attempts taken (1 = succeeded first try, 2 = one retry, etc.)
    /// on success, or the last error if all retries are exhausted.
    ///
    /// # Errors
    ///
    /// Returns [`OptimisticCommandError`] if:
    /// - The aggregate rejects the command
    /// - Events cannot be serialized
    /// - Another writer modified the stream and all retries are exhausted
    /// - The store fails to persist events
    /// - The snapshot store fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// match repo.execute_with_retry::<Account, Deposit>(&id, &cmd, &meta, 3) {
    ///     Ok(attempts) => println!("Succeeded after {attempts} attempt(s)"),
    ///     Err(e) => eprintln!("Failed after retries: {e}"),
    /// }
    /// ```
    pub fn execute_with_retry<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
        max_retries: usize,
    ) -> RetryResult<A, S, SS>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        for attempt in 1..=max_retries {
            match self.execute_command::<A, Cmd>(id, command, metadata) {
                Ok(()) => return Ok(attempt),
                Err(OptimisticCommandError::Concurrency(_)) => {}
                Err(e) => return Err(e),
            }
        }
        // Final attempt after retries exhausted
        self.execute_command::<A, Cmd>(id, command, metadata)
            .map(|()| max_retries + 1)
    }
}
