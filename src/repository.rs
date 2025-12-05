//! Application service orchestration.
//!
//! `Repository` coordinates loading aggregates, invoking command handlers, and
//! appending resulting events to the store. Errors are wrapped in `CommandError`
//! for a single call-site surface.

use std::fmt;

use crate::{
    ProjectionEvent, SerializableEvent,
    aggregate::{Aggregate, AggregateBuilder, Handle},
    projection::{Projection, ProjectionBuilder, ProjectionError},
};
use crate::{codec::Codec, store::EventStore};

type RepositoryCommandResult<A, S> = Result<
    (),
    CommandError<
        <A as Aggregate>::Error,
        <S as EventStore>::Error,
        <<S as EventStore>::Codec as Codec>::Error,
    >,
>;

/// Coordinates loading aggregates and persisting the resulting events.
pub struct Repository<S>
where
    S: EventStore,
{
    pub(crate) store: S,
}

impl<S> Repository<S>
where
    S: EventStore,
{
    #[must_use]
    pub const fn new(store: S) -> Self {
        Self { store }
    }

    #[must_use]
    pub const fn event_store(&self) -> &S {
        &self.store
    }

    pub const fn event_store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Create a projection builder for flexible query-side loading.
    ///
    /// Use this to build read models by specifying which events to apply
    /// and which streams to load from.
    pub const fn build_projection<P>(&self) -> ProjectionBuilder<'_, S, P>
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
    pub const fn aggregate_builder<A>(&self) -> AggregateBuilder<'_, S, A>
    where
        A: Aggregate,
    {
        AggregateBuilder::new(self)
    }

    /// Execute a command on an aggregate instance.
    ///
    /// The aggregate is loaded from the event store, then the command is handled
    /// through the `Handle<C>` trait, and resulting events are persisted.
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
    ) -> RepositoryCommandResult<A, S>
    where
        A: Aggregate + Handle<C>,
        A::Id: fmt::Display,
        A::Event: ProjectionEvent + SerializableEvent,
        S::Metadata: Clone,
    {
        // Load the aggregate (event kinds are automatically determined from A::Event::EVENT_KINDS)
        let aggregate = self
            .aggregate_builder::<A>()
            .load(id)
            .map_err(CommandError::Projection)?;

        // Handle the command (aggregates are pure functions of state + command)
        let events = Handle::<C>::handle(&aggregate, command).map_err(CommandError::Aggregate)?;

        if events.is_empty() {
            return Ok(());
        }

        // Begin transaction and append events
        let aggregate_id = id.to_string();
        let mut tx = self.store.begin(A::KIND, &aggregate_id);

        for event in events {
            tx.append(event, metadata.clone())
                .map_err(CommandError::Codec)?;
        }

        // Commit transaction
        tx.commit().map_err(CommandError::Store)
    }
}

/// Error type produced when executing a command through the repository.
#[derive(Debug)]
pub enum CommandError<AggregateError, StoreError, CodecError> {
    Aggregate(AggregateError),
    Projection(ProjectionError<StoreError, CodecError>),
    Codec(CodecError),
    Store(StoreError),
}

impl<A, S, C> fmt::Display for CommandError<A, S, C>
where
    A: fmt::Display,
    S: fmt::Display,
    C: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aggregate(error) => write!(f, "aggregate rejected command: {error}"),
            Self::Projection(error) => write!(f, "failed to rebuild aggregate state: {error}"),
            Self::Codec(error) => write!(f, "failed to encode events: {error}"),
            Self::Store(error) => write!(f, "failed to persist events: {error}"),
        }
    }
}

impl<A, S, C> std::error::Error for CommandError<A, S, C>
where
    A: fmt::Debug + fmt::Display,
    S: fmt::Debug + fmt::Display + std::error::Error + 'static,
    C: fmt::Debug + fmt::Display + std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Aggregate(_) => None,
            Self::Projection(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::Store(error) => Some(error),
        }
    }
}
