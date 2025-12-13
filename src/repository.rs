//! Application service orchestration.
//!
//! `Repository` coordinates loading aggregates, invoking command handlers, and
//! appending resulting events to the store.
//!
//! Snapshot support is opt-in via [`SnapshotRepository`]. This keeps the default
//! repository lightweight: no snapshot load/serialize work and no serde bounds on
//! aggregate state unless snapshots are enabled.

use std::marker::PhantomData;

use thiserror::Error;

use crate::{
    aggregate::{Aggregate, AggregateBuilder, Handle, SnapshotableAggregate},
    codec::{Codec, ProjectionEvent, SerializableEvent},
    concurrency::{ConcurrencyConflict, ConcurrencyStrategy, Optimistic, Unchecked},
    projection::{Projection, ProjectionBuilder, ProjectionError},
    snapshot::{Snapshot, SnapshotStore},
    store::{AppendError, EventFilter, EventStore},
};

type LoadError<S> =
    ProjectionError<<S as EventStore>::Error, <<S as EventStore>::Codec as Codec>::Error>;

/// Error type for unchecked command execution (no concurrency variant).
#[derive(Debug, Error)]
pub enum CommandError<AggregateError, StoreError, CodecError>
where
    StoreError: std::error::Error + 'static,
    CodecError: std::error::Error + 'static,
{
    #[error("aggregate rejected command: {0}")]
    Aggregate(AggregateError),
    #[error("failed to rebuild aggregate state: {0}")]
    Projection(#[source] ProjectionError<StoreError, CodecError>),
    #[error("failed to encode events: {0}")]
    Codec(#[source] CodecError),
    #[error("failed to persist events: {0}")]
    Store(#[source] StoreError),
}

/// Error type for snapshot-enabled unchecked command execution.
#[derive(Debug, Error)]
pub enum SnapshotCommandError<AggregateError, StoreError, CodecError, SnapshotError>
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
pub enum OptimisticCommandError<AggregateError, Position, StoreError, CodecError>
where
    Position: std::fmt::Debug,
    StoreError: std::error::Error + 'static,
    CodecError: std::error::Error + 'static,
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
}

/// Error type for snapshot-enabled optimistic command execution (includes concurrency).
#[derive(Debug, Error)]
pub enum OptimisticSnapshotCommandError<
    AggregateError,
    Position,
    StoreError,
    CodecError,
    SnapshotError,
> where
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
pub type UncheckedCommandResult<A, S> = Result<
    (),
    CommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
    >,
>;

/// Result type alias for snapshot-enabled unchecked command execution.
pub type UncheckedSnapshotCommandResult<A, S, SS> = Result<
    (),
    SnapshotCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

/// Result type alias for optimistic command execution.
pub type OptimisticCommandResult<A, S> = Result<
    (),
    OptimisticCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Position,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
    >,
>;

/// Result type alias for snapshot-enabled optimistic command execution.
pub type OptimisticSnapshotCommandResult<A, S, SS> = Result<
    (),
    OptimisticSnapshotCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Position,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

/// Result type alias for retry operations (optimistic, no snapshots).
pub type RetryResult<A, S> = Result<
    usize,
    OptimisticCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Position,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
    >,
>;

/// Result type alias for retry operations (optimistic, snapshots enabled).
pub type SnapshotRetryResult<A, S, SS> = Result<
    usize,
    OptimisticSnapshotCommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Position,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
        <SS as SnapshotStore>::Error,
    >,
>;

struct LoadedAggregate<A, Pos> {
    aggregate: A,
    version: Option<Pos>,
    events_since_snapshot: u64,
}

/// Repository with no snapshot support.
pub struct Repository<S, C = Optimistic>
where
    S: EventStore,
    C: ConcurrencyStrategy,
{
    pub(crate) store: S,
    _concurrency: PhantomData<C>,
}

/// Repository with snapshot support.
pub struct SnapshotRepository<S, SS, C = Optimistic>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    C: ConcurrencyStrategy,
{
    pub(crate) store: S,
    snapshots: SS,
    _concurrency: PhantomData<C>,
}

impl<S> Repository<S, Optimistic>
where
    S: EventStore,
{
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self {
            store,
            _concurrency: PhantomData,
        }
    }

    /// Disable optimistic concurrency checking for this repository.
    #[must_use]
    pub fn without_concurrency_checking(self) -> Repository<S, Unchecked> {
        Repository {
            store: self.store,
            _concurrency: PhantomData,
        }
    }
}

impl<S, C> Repository<S, C>
where
    S: EventStore,
    C: ConcurrencyStrategy,
{
    #[must_use]
    pub const fn event_store(&self) -> &S {
        &self.store
    }

    pub fn build_projection<P>(&self) -> ProjectionBuilder<'_, S, P>
    where
        P: Projection<Id = S::Id>,
    {
        ProjectionBuilder::new(&self.store)
    }

    pub const fn aggregate_builder<A>(&self) -> AggregateBuilder<'_, Self, A>
    where
        A: Aggregate<Id = S::Id>,
    {
        AggregateBuilder::new(self)
    }

    #[must_use]
    pub fn with_snapshots<SS>(self, snapshots: SS) -> SnapshotRepository<S, SS, C>
    where
        SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    {
        SnapshotRepository {
            store: self.store,
            snapshots,
            _concurrency: PhantomData,
        }
    }

    /// Load an aggregate by replaying all events (no snapshots).
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError`] if the store fails to load events or if an event cannot be
    /// decoded into the aggregate's event sum type.
    pub fn load<A>(&self, id: &S::Id) -> Result<A, LoadError<S>>
    where
        A: Aggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
    {
        Ok(self.load_aggregate::<A>(id)?.aggregate)
    }

    fn load_aggregate<A>(&self, id: &S::Id) -> Result<LoadedAggregate<A, S::Position>, LoadError<S>>
    where
        A: Aggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
    {
        let filters: Vec<EventFilter<S::Id, S::Position>> = A::Event::EVENT_KINDS
            .iter()
            .map(|kind| EventFilter::for_aggregate(*kind, A::KIND, id.clone()))
            .collect();

        let events = self
            .store
            .load_events(&filters)
            .map_err(ProjectionError::Store)?;

        let codec = self.store.codec();
        let mut aggregate = A::default();
        let mut version: Option<S::Position> = None;

        for stored in &events {
            let event = A::Event::from_stored(&stored.kind, &stored.data, codec)
                .map_err(ProjectionError::EventDecode)?;
            aggregate.apply(&event);
            version = Some(stored.position);
        }

        Ok(LoadedAggregate {
            aggregate,
            version,
            events_since_snapshot: events.len() as u64,
        })
    }
}

impl<S> Repository<S, Unchecked>
where
    S: EventStore,
{
    /// Execute a command with last-writer-wins semantics (no concurrency checking).
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when the aggregate rejects the command, events cannot be encoded,
    /// the store fails to persist, or the aggregate cannot be rebuilt.
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> UncheckedCommandResult<A, S>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        let LoadedAggregate { aggregate, .. } = self
            .load_aggregate::<A>(id)
            .map_err(CommandError::Projection)?;

        let new_events = match Handle::<Cmd>::handle(&aggregate, command) {
            Ok(events) => events,
            Err(e) => return Err(CommandError::Aggregate(e)),
        };

        if new_events.is_empty() {
            return Ok(());
        }

        let mut aggregate = aggregate;
        for event in &new_events {
            aggregate.apply(event);
        }

        let mut tx = self.store.begin::<Unchecked>(A::KIND, id.clone(), None);
        for event in new_events {
            tx.append(event, metadata.clone())
                .map_err(CommandError::Codec)?;
        }
        tx.commit().map_err(CommandError::Store)?;
        Ok(())
    }
}

impl<S> Repository<S, Optimistic>
where
    S: EventStore,
{
    /// Execute a command using optimistic concurrency control.
    ///
    /// # Errors
    ///
    /// Returns [`OptimisticCommandError::Concurrency`] if the stream version changed between
    /// loading and committing. Other variants cover aggregate validation, encoding, persistence,
    /// and projection rebuild errors.
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> OptimisticCommandResult<A, S>
    where
        A: Aggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        let LoadedAggregate {
            aggregate, version, ..
        } = self
            .load_aggregate::<A>(id)
            .map_err(OptimisticCommandError::Projection)?;

        let new_events = match Handle::<Cmd>::handle(&aggregate, command) {
            Ok(events) => events,
            Err(e) => return Err(OptimisticCommandError::Aggregate(e)),
        };

        if new_events.is_empty() {
            return Ok(());
        }

        let mut aggregate = aggregate;
        for event in &new_events {
            aggregate.apply(event);
        }

        let mut tx = self.store.begin::<Optimistic>(A::KIND, id.clone(), version);
        for event in new_events {
            tx.append(event, metadata.clone())
                .map_err(OptimisticCommandError::Codec)?;
        }

        if let Err(e) = tx.commit() {
            match e {
                AppendError::Conflict(c) => return Err(OptimisticCommandError::Concurrency(c)),
                AppendError::Store(s) => return Err(OptimisticCommandError::Store(s)),
            }
        }

        Ok(())
    }

    /// Execute a command with automatic retry on concurrency conflicts.
    ///
    /// # Errors
    ///
    /// Returns the last error if all retries are exhausted, or a non-concurrency error immediately.
    pub fn execute_with_retry<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
        max_retries: usize,
    ) -> RetryResult<A, S>
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

        self.execute_command::<A, Cmd>(id, command, metadata)
            .map(|()| max_retries + 1)
    }
}

impl<S, SS> SnapshotRepository<S, SS, Optimistic>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
    #[must_use]
    pub fn without_concurrency_checking(self) -> SnapshotRepository<S, SS, Unchecked> {
        SnapshotRepository {
            store: self.store,
            snapshots: self.snapshots,
            _concurrency: PhantomData,
        }
    }
}

impl<S, SS, C> SnapshotRepository<S, SS, C>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    C: ConcurrencyStrategy,
{
    #[must_use]
    pub const fn event_store(&self) -> &S {
        &self.store
    }

    #[must_use]
    pub const fn snapshot_store(&self) -> &SS {
        &self.snapshots
    }

    pub fn build_projection<P>(&self) -> ProjectionBuilder<'_, S, P>
    where
        P: Projection<Id = S::Id>,
    {
        ProjectionBuilder::new(&self.store)
    }

    pub const fn aggregate_builder<A>(&self) -> AggregateBuilder<'_, Self, A>
    where
        A: SnapshotableAggregate<Id = S::Id>,
    {
        AggregateBuilder::new(self)
    }

    /// Load an aggregate using snapshots when available.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError`] if the store fails to load events, if an event cannot be
    /// decoded, or if a stored snapshot cannot be deserialized (which indicates snapshot
    /// corruption).
    pub fn load<A>(&self, id: &S::Id) -> Result<A, LoadError<S>>
    where
        A: SnapshotableAggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
    {
        Ok(self.load_aggregate::<A>(id)?.aggregate)
    }

    fn load_aggregate<A>(&self, id: &S::Id) -> Result<LoadedAggregate<A, S::Position>, LoadError<S>>
    where
        A: SnapshotableAggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
    {
        let codec = self.store.codec();

        let snapshot_result = match self.snapshots.load(A::KIND, id) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to load snapshot, falling back to full replay"
                );
                None
            }
        };

        let (mut aggregate, snapshot_position) = if let Some(snapshot) = snapshot_result {
            let restored: A = codec
                .deserialize(&snapshot.data)
                .map_err(ProjectionError::SnapshotDeserialize)?;
            (restored, Some(snapshot.position))
        } else {
            (A::default(), None)
        };

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

        let events = self
            .store
            .load_events(&filters)
            .map_err(ProjectionError::Store)?;

        let mut last_event_position: Option<S::Position> = None;
        for stored in &events {
            let event = A::Event::from_stored(&stored.kind, &stored.data, codec)
                .map_err(ProjectionError::EventDecode)?;
            aggregate.apply(&event);
            last_event_position = Some(stored.position);
        }

        let version = last_event_position.or(snapshot_position);

        Ok(LoadedAggregate {
            aggregate,
            version,
            events_since_snapshot: events.len() as u64,
        })
    }
}

impl<S, SS> SnapshotRepository<S, SS, Unchecked>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
    /// Execute a command with last-writer-wins semantics and optional snapshotting.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotCommandError`] when the aggregate rejects the command, events cannot be
    /// encoded, the store fails to persist, snapshot persistence fails, or the aggregate cannot be
    /// rebuilt.
    ///
    /// # Panics
    ///
    /// Panics if the store reports `None` from `stream_version` after a successful append. This
    /// indicates a bug in the event store implementation.
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> UncheckedSnapshotCommandResult<A, S, SS>
    where
        A: SnapshotableAggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        let LoadedAggregate {
            aggregate,
            events_since_snapshot,
            ..
        } = self
            .load_aggregate::<A>(id)
            .map_err(SnapshotCommandError::Projection)?;

        let new_events = match Handle::<Cmd>::handle(&aggregate, command) {
            Ok(events) => events,
            Err(e) => return Err(SnapshotCommandError::Aggregate(e)),
        };

        let events_count = new_events.len();
        if events_count == 0 {
            return Ok(());
        }

        let mut aggregate = aggregate;
        for event in &new_events {
            aggregate.apply(event);
        }

        let mut tx = self.store.begin::<Unchecked>(A::KIND, id.clone(), None);
        for event in new_events {
            tx.append(event, metadata.clone())
                .map_err(SnapshotCommandError::Codec)?;
        }
        tx.commit().map_err(SnapshotCommandError::Store)?;

        let total_events_since_snapshot = events_since_snapshot + events_count as u64;
        if !self
            .snapshots
            .should_snapshot(A::KIND, id, total_events_since_snapshot)
        {
            return Ok(());
        }

        let new_position = self
            .store
            .stream_version(A::KIND, id)
            .map_err(SnapshotCommandError::Store)?
            .expect("stream should have events after append");

        let codec = self.store.codec();
        let snapshot = Snapshot {
            position: new_position,
            data: codec
                .serialize(&aggregate)
                .map_err(SnapshotCommandError::Codec)?,
        };

        self.snapshots
            .offer_snapshot(A::KIND, id, snapshot, total_events_since_snapshot)
            .map_err(SnapshotCommandError::Snapshot)?;

        Ok(())
    }
}

impl<S, SS> SnapshotRepository<S, SS, Optimistic>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
{
    /// Execute a command using optimistic concurrency control and optional snapshotting.
    ///
    /// # Errors
    ///
    /// Returns [`OptimisticSnapshotCommandError::Concurrency`] if the stream version changed
    /// between loading and committing. Other variants cover aggregate validation, encoding,
    /// persistence, snapshot persistence, and projection rebuild errors.
    ///
    /// # Panics
    ///
    /// Panics if the store reports `None` from `stream_version` after a successful append. This
    /// indicates a bug in the event store implementation.
    pub fn execute_command<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
    ) -> OptimisticSnapshotCommandResult<A, S, SS>
    where
        A: SnapshotableAggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        let LoadedAggregate {
            aggregate,
            version,
            events_since_snapshot,
        } = self
            .load_aggregate::<A>(id)
            .map_err(OptimisticSnapshotCommandError::Projection)?;

        let new_events = match Handle::<Cmd>::handle(&aggregate, command) {
            Ok(events) => events,
            Err(e) => return Err(OptimisticSnapshotCommandError::Aggregate(e)),
        };

        let events_count = new_events.len();
        if events_count == 0 {
            return Ok(());
        }

        let mut aggregate = aggregate;
        for event in &new_events {
            aggregate.apply(event);
        }

        let mut tx = self.store.begin::<Optimistic>(A::KIND, id.clone(), version);
        for event in new_events {
            tx.append(event, metadata.clone())
                .map_err(OptimisticSnapshotCommandError::Codec)?;
        }

        if let Err(e) = tx.commit() {
            match e {
                AppendError::Conflict(c) => {
                    return Err(OptimisticSnapshotCommandError::Concurrency(c));
                }
                AppendError::Store(s) => return Err(OptimisticSnapshotCommandError::Store(s)),
            }
        }

        let total_events_since_snapshot = events_since_snapshot + events_count as u64;
        if !self
            .snapshots
            .should_snapshot(A::KIND, id, total_events_since_snapshot)
        {
            return Ok(());
        }

        let new_position = self
            .store
            .stream_version(A::KIND, id)
            .map_err(OptimisticSnapshotCommandError::Store)?
            .expect("stream should have events after append");

        let codec = self.store.codec();
        let snapshot = Snapshot {
            position: new_position,
            data: codec
                .serialize(&aggregate)
                .map_err(OptimisticSnapshotCommandError::Codec)?,
        };

        self.snapshots
            .offer_snapshot(A::KIND, id, snapshot, total_events_since_snapshot)
            .map_err(OptimisticSnapshotCommandError::Snapshot)?;

        Ok(())
    }

    /// Execute a command with automatic retry on concurrency conflicts.
    ///
    /// # Errors
    ///
    /// Returns the last error if all retries are exhausted, or a non-concurrency error immediately.
    pub fn execute_with_retry<A, Cmd>(
        &mut self,
        id: &S::Id,
        command: &Cmd,
        metadata: &S::Metadata,
        max_retries: usize,
    ) -> SnapshotRetryResult<A, S, SS>
    where
        A: SnapshotableAggregate<Id = S::Id> + Handle<Cmd>,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        for attempt in 1..=max_retries {
            match self.execute_command::<A, Cmd>(id, command, metadata) {
                Ok(()) => return Ok(attempt),
                Err(OptimisticSnapshotCommandError::Concurrency(_)) => {}
                Err(e) => return Err(e),
            }
        }

        self.execute_command::<A, Cmd>(id, command, metadata)
            .map(|()| max_retries + 1)
    }
}
