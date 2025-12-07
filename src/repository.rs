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

use std::fmt;

use crate::{
    ProjectionEvent, SerializableEvent,
    aggregate::{Aggregate, AggregateBuilder, Handle},
    codec::Codec,
    projection::{Projection, ProjectionBuilder, ProjectionError},
    snapshot::{NoSnapshots, Snapshot, SnapshotStore},
    store::{EventFilter, EventStore},
};

type RepositoryCommandResult<A, S, SS> = Result<
    (),
    CommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

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
/// - `SS`: Snapshot store implementation (defaults to [`NoSnapshots`])
pub struct Repository<S, SS = NoSnapshots>
where
    S: EventStore,
    SS: SnapshotStore,
{
    pub(crate) store: S,
    snapshots: SS,
}

impl<S> Repository<S, NoSnapshots>
where
    S: EventStore,
{
    /// Create a new repository without snapshot support.
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self {
            store,
            snapshots: NoSnapshots,
        }
    }
}

impl<S, SS> Repository<S, SS>
where
    S: EventStore,
    SS: SnapshotStore,
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
    pub fn with_snapshots<NewSS: SnapshotStore>(self, snapshots: NewSS) -> Repository<S, NewSS> {
        Repository {
            store: self.store,
            snapshots,
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
    pub const fn build_projection<P>(&self) -> ProjectionBuilder<'_, S, SS, P>
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
    pub const fn aggregate_builder<A>(&self) -> AggregateBuilder<'_, S, SS, A>
    where
        A: Aggregate,
    {
        AggregateBuilder::new(self)
    }

    /// Execute a command on an aggregate instance.
    ///
    /// The aggregate is loaded from the event store (optionally using a snapshot),
    /// then the command is handled through the `Handle<C>` trait, and resulting
    /// events are persisted. If a snapshot store is configured, a new snapshot
    /// may be created based on the store's policy.
    ///
    /// Metadata can be provided to enrich events with causation tracking, timestamps, etc.
    /// The metadata type is determined by the event store.
    ///
    /// # Type Parameters
    ///
    /// - `A`: The aggregate type
    /// - `C`: The command type (must implement `Handle<C> for A`)
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if:
    /// - The aggregate cannot be loaded
    /// - The aggregate rejects the command
    /// - Events cannot be serialized
    /// - The store fails to persist events
    /// - The snapshot store fails (note: snapshot failures are typically non-fatal)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let command = Restock { quantity: 100, unit_price_cents: 2_500 };
    /// repository.execute_command::<Product, Restock>(&product_id, &command, &EventMetadata::default())?;
    /// ```
    pub fn execute_command<A, C>(
        &mut self,
        id: &A::Id,
        command: &C,
        metadata: &S::Metadata,
    ) -> RepositoryCommandResult<A, S, SS>
    where
        A: Aggregate + Handle<C>,
        A::Id: fmt::Display,
        A::Event: ProjectionEvent + SerializableEvent + Clone,
        S::Metadata: Clone,
        S::Position: Into<u64> + Copy,
    {
        let aggregate_id = id.to_string();
        let codec = self.store.codec();

        // Try to load snapshot
        let snapshot_result = self
            .snapshots
            .load(A::KIND, &aggregate_id)
            .map_err(CommandError::Snapshot)?;

        // Restore aggregate from snapshot or start fresh
        let (mut aggregate, snapshot_position) = match snapshot_result {
            Some(snapshot) => {
                // Try to deserialize snapshot; fall back to default on failure
                match serde_json::from_slice::<A>(&snapshot.data) {
                    Ok(restored) => (restored, Some(snapshot.position)),
                    Err(_) => (A::default(), None),
                }
            }
            None => (A::default(), None),
        };

        // Build filters for events after snapshot position
        let filters: Vec<EventFilter> = A::Event::EVENT_KINDS
            .iter()
            .map(|kind| {
                let mut filter = EventFilter::for_aggregate(*kind, A::KIND, aggregate_id.clone());
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
            .map_err(ProjectionError::Store)
            .map_err(CommandError::Projection)?;

        let mut events_replayed = 0u64;
        let mut last_position = snapshot_position.unwrap_or(0);

        for stored in &events {
            let event = A::Event::from_stored(&stored.kind, &stored.data, codec)
                .map_err(ProjectionError::Codec)
                .map_err(CommandError::Projection)?;
            aggregate.apply(&event);
            events_replayed += 1;
            last_position = stored.position.into();
        }

        // Handle the command (aggregates are pure functions of state + command)
        let new_events =
            Handle::<C>::handle(&aggregate, command).map_err(CommandError::Aggregate)?;

        if new_events.is_empty() {
            return Ok(());
        }

        // Begin transaction and append events
        let mut tx = self.store.begin(A::KIND, &aggregate_id);

        for event in &new_events {
            tx.append(event.clone(), metadata.clone())
                .map_err(CommandError::Codec)?;
        }

        // Commit transaction
        tx.commit().map_err(CommandError::Store)?;

        // Apply new events to aggregate for potential snapshotting
        for event in new_events {
            aggregate.apply(&event);
            last_position += 1; // Approximate new positions
        }

        // Offer snapshot to the snapshot store (it decides whether to save)
        let snapshot = Snapshot {
            position: last_position,
            data: serde_json::to_vec(&aggregate).unwrap_or_default(),
        };

        self.snapshots
            .offer_snapshot(A::KIND, &aggregate_id, snapshot, events_replayed)
            .map_err(CommandError::Snapshot)?;

        Ok(())
    }
}

/// Error type produced when executing a command through the repository.
#[derive(Debug)]
pub enum CommandError<AggregateError, StoreError, CodecError, SnapshotError> {
    Aggregate(AggregateError),
    Projection(ProjectionError<StoreError, CodecError>),
    Codec(CodecError),
    Store(StoreError),
    Snapshot(SnapshotError),
}

impl<A, S, C, SS> fmt::Display for CommandError<A, S, C, SS>
where
    A: fmt::Display,
    S: fmt::Display,
    C: fmt::Display,
    SS: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aggregate(error) => write!(f, "aggregate rejected command: {error}"),
            Self::Projection(error) => write!(f, "failed to rebuild aggregate state: {error}"),
            Self::Codec(error) => write!(f, "failed to encode events: {error}"),
            Self::Store(error) => write!(f, "failed to persist events: {error}"),
            Self::Snapshot(error) => write!(f, "snapshot operation failed: {error}"),
        }
    }
}

impl<A, S, C, SS> std::error::Error for CommandError<A, S, C, SS>
where
    A: fmt::Debug + fmt::Display,
    S: fmt::Debug + fmt::Display + std::error::Error + 'static,
    C: fmt::Debug + fmt::Display + std::error::Error + 'static,
    SS: fmt::Debug + fmt::Display + std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Aggregate(_) => None,
            Self::Projection(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Snapshot(error) => Some(error),
        }
    }
}
