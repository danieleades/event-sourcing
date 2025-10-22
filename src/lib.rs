#![doc = include_str!("../README.md")]

use serde::{Serialize, de::DeserializeOwned};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::marker::PhantomData;

// Re-export the derive macros so users only need one import
pub use event_sourcing_macros::Aggregate;

/// Marker trait for events that can be persisted by the event store.
///
/// Each event carries a unique [`Self::KIND`] identifier so the repository can route
/// stored bytes back to the correct type when rebuilding aggregates or projections.
///
/// Most projects implement this trait by hand, but the `#[derive(Aggregate)]` macro
/// generates it automatically for the aggregate event enums it creates.
pub trait DomainEvent: Serialize + DeserializeOwned {
    const KIND: &'static str;
}

/// Mutate an aggregate with a domain event.
///
/// `Apply<E>` is called while the repository rebuilds aggregate state, keeping the domain
/// logic focused on pure events rather than persistence concerns.
///
/// ```ignore
/// #[derive(Default)]
/// struct Account {
///     balance: i64,
/// }
///
/// impl Apply<FundsDeposited> for Account {
///     fn apply(&mut self, event: &FundsDeposited) {
///         self.balance += event.amount;
///     }
/// }
/// ```
pub trait Apply<E> {
    fn apply(&mut self, event: &E);
}

/// Entry point for command handling.
///
/// Each command type gets its own implementation, letting the aggregate express validation
/// logic in a strongly typed way.
///
/// ```ignore
/// impl Handle<DepositFunds> for Account {
///     fn handle(&self, command: &DepositFunds) -> Result<Vec<Self::Event>, Self::Error> {
///         if command.amount <= 0 {
///             return Err("amount must be positive".into());
///         }
///         Ok(vec![FundsDeposited { amount: command.amount }.into()])
///     }
/// }
/// ```
pub trait Handle<C>: Aggregate {
    /// Handle a command and produce events.
    ///
    /// Aggregates are pure functions of state and command.
    /// The aggregate ID is infrastructure metadata, not needed for business logic.
    ///
    /// # Errors
    ///
    /// Returns an error when the command cannot be handled due to business rule violations.
    fn handle(&self, command: &C) -> Result<Vec<Self::Event>, Self::Error>;
}

/// Apply an event to a projection with access to envelope context.
///
/// Implementations receive the aggregate identifier (already stripped of its kind prefix),
/// the pure domain event, and metadata supplied by the backing store.
///
/// ```ignore
/// impl ApplyProjection<InventoryAdjusted, ()> for InventoryReport {
///     fn apply_projection(&mut self, aggregate_id: &str, event: &InventoryAdjusted, _metadata: &()) {
///         let sku = aggregate_id; // "product::" prefix was removed
///         let stats = self.products.entry(sku.to_string()).or_default();
///         stats.quantity += event.delta;
///     }
/// }
/// ```
pub trait ApplyProjection<E, M = EventMetadata> {
    fn apply_projection(&mut self, aggregate_id: &str, event: &E, metadata: &M);
}

/// Metadata attached to events when they are persisted.
///
/// This separates infrastructure concerns (timestamps, causation tracking, etc.)
/// from pure domain events.
#[derive(Debug, Clone, Default)]
pub struct EventMetadata {
    /// When the event was created/committed
    pub timestamp: Option<std::time::SystemTime>,
    /// ID of the command that caused this event
    pub causation_id: Option<String>,
    /// Business transaction/correlation ID
    pub correlation_id: Option<String>,
    /// User/system that triggered the event
    pub user_id: Option<String>,
}

/// Wraps a domain event with its aggregate identifier and metadata.
///
/// Aggregates operate on bare events through [`Apply`], while projections normally receive
/// the richer `EventEnvelope` so they can correlate across streams or inspect metadata.
///
/// The derive macros call this type automatically when rebuilding projections; most users
/// only need the convenience accessors (`aggregate_id`, [`stream_id`](Self::stream_id)).
#[derive(Debug, Clone)]
pub struct EventEnvelope<E, M = EventMetadata> {
    /// Aggregate type identifier (from `Aggregate::KIND`)
    pub aggregate_kind: String,
    /// Aggregate instance identifier
    pub aggregate_id: String,
    /// The pure domain event
    pub event: E,
    /// Infrastructure metadata
    pub metadata: M,
}

/// Trait implemented by read models that can be constructed from an event stream.
///
/// Implementors specify the metadata type their [`ApplyProjection`] handlers expect.
/// Projections are typically rebuilt by calling [`Repository::build_projection`] and
/// configuring the desired event streams before invoking [`ProjectionBuilder::load`].
pub trait Projection: Default + Sized {
    /// Metadata type expected by this projection
    type Metadata;
}

/// Trait for event sum types that can serialize themselves for persistence.
///
/// Implemented by aggregate event enums to serialize variants for persistence.
///
/// The `Aggregate` derive macro generates this automatically; hand-written enums can
/// implement it directly if preferred. The generic `M` parameter allows passing through
/// custom metadata types supplied by the store.
pub trait SerializableEvent {
    /// Serialize this event to persistable form with generic metadata.
    ///
    /// # Errors
    ///
    /// Returns a codec error if serialization fails.
    fn to_persistable<C: Codec, M>(
        self,
        codec: &C,
        metadata: M,
    ) -> Result<PersistableEvent<M>, C::Error>;
}

/// Trait for event sum types that can deserialize themselves from stored events.
///
/// Implemented by event enums to deserialize stored events.
///
/// Deriving [`Aggregate`](crate::Aggregate) includes a `ProjectionEvent` implementation for
/// the generated sum type. Custom enums can opt in manually using the pattern illustrated
/// below.
///
/// # Example
///
/// ```ignore
/// enum AccountEvent {
///     FundsDeposited(FundsDeposited),
///     FundsWithdrawn(FundsWithdrawn),
/// }
///
/// impl ProjectionEvent for AccountEvent {
///     const EVENT_KINDS: &'static [&'static str] = &[
///         FundsDeposited::KIND,
///         FundsWithdrawn::KIND,
///     ];
///
///     fn from_stored<C: Codec>(
///         kind: &str,
///         data: &[u8],
///         codec: &C,
///     ) -> Result<Self, C::Error> {
///         match kind {
///             FundsDeposited::KIND => {
///                 Ok(Self::FundsDeposited(codec.deserialize(data)?))
///             }
///             FundsWithdrawn::KIND => {
///                 Ok(Self::FundsWithdrawn(codec.deserialize(data)?))
///             }
///             _ => panic!("Unknown event kind: {}", kind),
///         }
///     }
/// }
/// ```
pub trait ProjectionEvent: Sized {
    /// The list of event kinds this sum type can deserialize.
    ///
    /// This data is generated automatically when using the derive macros.
    const EVENT_KINDS: &'static [&'static str];

    /// Deserialize an event from stored representation.
    ///
    /// # Errors
    ///
    /// Returns a codec error if deserialization fails.
    fn from_stored<C: Codec>(kind: &str, data: &[u8], codec: &C) -> Result<Self, C::Error>;
}

/// Raw event data ready to be written to a store backend.
///
/// This is the boundary between Repository and `EventStore`. Repository serializes
/// events to this form, `EventStore` adds position and persistence.
///
/// Generic over metadata type `M` to support different metadata structures.
#[derive(Clone)]
pub struct PersistableEvent<M = EventMetadata> {
    pub kind: String,
    pub data: Vec<u8>,
    pub metadata: M,
}

/// Event materialized from the store, with position.
///
/// Generic parameters:
/// - `Pos`: Position type for ordering (`()` for no ordering, `u64` for global sequence, etc.)
/// - `M`: Metadata type (defaults to `EventMetadata`)
#[derive(Clone)]
pub struct StoredEvent<Pos = (), M = EventMetadata> {
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub kind: String,
    pub position: Pos,
    pub data: Vec<u8>,
    pub metadata: M,
}

/// Filter describing which events should be loaded from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventFilter {
    pub event_kind: String,
    pub aggregate_kind: Option<String>,
    pub aggregate_id: Option<String>,
}

impl EventFilter {
    /// Load all events of the specified kind across every aggregate.
    #[must_use]
    pub fn for_event(kind: impl Into<String>) -> Self {
        Self {
            event_kind: kind.into(),
            aggregate_kind: None,
            aggregate_id: None,
        }
    }

    /// Load events of the specified kind for a single aggregate instance.
    #[must_use]
    pub fn for_aggregate(
        event_kind: impl Into<String>,
        aggregate_kind: impl Into<String>,
        aggregate_id: impl Into<String>,
    ) -> Self {
        Self {
            event_kind: event_kind.into(),
            aggregate_kind: Some(aggregate_kind.into()),
            aggregate_id: Some(aggregate_id.into()),
        }
    }
}

/// Convenience alias for event batches loaded from a store.
pub type LoadEventsResult<Pos, Meta, Err> = Result<Vec<StoredEvent<Pos, Meta>>, Err>;

/// Serialisation strategy used by event stores.
pub trait Codec {
    type Error;

    /// # Errors
    ///
    /// Returns an error from the codec if the event cannot be serialized.
    fn serialize<E>(&self, event: &E) -> Result<Vec<u8>, Self::Error>
    where
        E: DomainEvent;

    /// # Errors
    ///
    /// Returns an error from the codec if the bytes cannot be decoded.
    fn deserialize<E>(&self, data: &[u8]) -> Result<E, Self::Error>
    where
        E: DomainEvent;
}

/// Transaction for appending events to an aggregate instance.
///
/// Events are accumulated in the transaction and persisted atomically when `commit()` is called.
/// If the transaction is dropped without calling `commit()`, the events are silently discarded
/// (rolled back). This allows errors during event serialization to be handled gracefully.
pub struct Transaction<'a, S: EventStore> {
    store: &'a mut S,
    aggregate_kind: String,
    aggregate_id: String,
    events: Vec<PersistableEvent<S::Metadata>>,
    committed: bool,
}

impl<'a, S: EventStore> Transaction<'a, S> {
    const fn new(store: &'a mut S, aggregate_kind: String, aggregate_id: String) -> Self {
        Self {
            store,
            aggregate_kind,
            aggregate_id,
            events: Vec::new(),
            committed: false,
        }
    }

    /// Append an event to the transaction.
    ///
    /// For sum-type events (enums), this serializes each variant to its persistable form.
    ///
    /// # Errors
    ///
    /// Returns a codec error if serialization fails.
    pub fn append<E>(
        &mut self,
        event: E,
        metadata: S::Metadata,
    ) -> Result<(), <S::Codec as Codec>::Error>
    where
        E: SerializableEvent,
    {
        let persistable = event.to_persistable(self.store.codec(), metadata)?;
        self.events.push(persistable);
        Ok(())
    }

    /// Commit the transaction, persisting all events atomically.
    ///
    /// # Errors
    ///
    /// Returns a store error if persistence fails.
    pub fn commit(mut self) -> Result<(), S::Error> {
        let events = std::mem::take(&mut self.events);
        self.committed = true;
        self.store
            .append_batch(&self.aggregate_kind, &self.aggregate_id, events)
    }
}

impl<S: EventStore> Drop for Transaction<'_, S> {
    fn drop(&mut self) {
        // Silently discard uncommitted events (rollback).
        // This allows codec/validation errors during append() to be handled gracefully
        // without panicking. The events were never persisted, so this is safe.
    }
}

/// Abstraction over the persistence layer for event streams.
///
/// This trait supports both stream-based and type-partitioned storage implementations.
///
/// Associated types allow stores to customize their behavior:
/// - `Position`: Ordering strategy (`()` for stream-based, `u64` for global ordering)
/// - `Metadata`: Infrastructure metadata type (timestamps, causation tracking, etc.)
/// - `Codec`: Serialization strategy for domain events
pub trait EventStore {
    /// Position type used for ordering events (use `()` if not needed)
    type Position;
    /// Store-specific error type
    type Error;
    /// Serialization codec
    type Codec: Codec;
    /// Metadata type for infrastructure concerns
    type Metadata;

    fn codec(&self) -> &Self::Codec;

    /// Begin a transaction for appending events to an aggregate.
    ///
    /// # Arguments
    /// * `aggregate_kind` - The aggregate type identifier (`Aggregate::KIND`)
    /// * `aggregate_id` - The aggregate instance identifier
    fn begin(&mut self, aggregate_kind: &str, aggregate_id: &str) -> Transaction<'_, Self>
    where
        Self: Sized;

    /// Internal method called by Transaction to persist events.
    ///
    /// Implementations should assign positions and store events atomically.
    ///
    /// # Errors
    ///
    /// Returns a store-specific error when persisting fails.
    fn append_batch(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &str,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), Self::Error>;

    /// Load events matching the specified filters.
    ///
    /// Each filter describes an event kind and optional aggregate identity:
    /// - [`EventFilter::for_event`] loads every event of the given kind
    /// - [`EventFilter::for_aggregate`] narrows to a single aggregate instance
    ///
    /// The store optimizes based on its storage model and returns events
    /// merged by position (if positions are available).
    ///
    /// # Errors
    ///
    /// Returns a store-specific error when loading fails.
    fn load_events(
        &self,
        filters: &[EventFilter],
    ) -> LoadEventsResult<Self::Position, Self::Metadata, Self::Error>;
}

/// Type alias for event handler closures.
type EventHandler<P, S> = Box<
    dyn Fn(
        &mut P,
        &str,
        &[u8],
        &<S as EventStore>::Metadata,
        &<S as EventStore>::Codec,
    ) -> Result<(), <<S as EventStore>::Codec as Codec>::Error>,
>;

/// Internal type pairing an event filter with its dispatch handler.
struct RegisteredEvent<P, S: EventStore> {
    filter: EventFilter,
    apply: EventHandler<P, S>,
}

/// Builder used to configure which events should be loaded for a projection.
pub struct ProjectionBuilder<'a, S, P>
where
    S: EventStore,
    P: Projection,
{
    repository: &'a Repository<S>,
    registered: Vec<RegisteredEvent<P, S>>,
    _phantom: PhantomData<P>,
}

impl<'a, S, P> ProjectionBuilder<'a, S, P>
where
    S: EventStore,
    P: Projection,
{
    #[allow(clippy::missing_const_for_fn)]
    fn new(repository: &'a Repository<S>) -> Self {
        Self {
            repository,
            registered: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Register a specific event type to load from all aggregates.
    ///
    /// # Example
    /// ```ignore
    /// builder.event::<ProductRestocked>()  // All products
    /// ```
    #[must_use]
    pub fn event<E>(mut self) -> Self
    where
        E: DomainEvent,
        P: ApplyProjection<E, P::Metadata>,
        S::Metadata: Clone + Into<P::Metadata>,
    {
        self.registered.push(RegisteredEvent {
            filter: EventFilter::for_event(E::KIND),
            apply: Box::new(|proj, agg_id, data, metadata, codec| {
                let event: E = codec.deserialize(data)?;
                let metadata_converted: P::Metadata = metadata.clone().into();
                ApplyProjection::apply_projection(proj, agg_id, &event, &metadata_converted);
                Ok(())
            }),
        });
        self
    }

    /// Register a specific event type to load from a specific aggregate instance.
    ///
    /// # Example
    /// ```ignore
    /// builder.event_for::<Account, FundsDeposited>(&account_id); // One account stream
    /// ```
    #[must_use]
    pub fn event_for<A, E>(mut self, aggregate_id: &A::Id) -> Self
    where
        A: Aggregate,
        A::Id: fmt::Display,
        E: DomainEvent,
        P: ApplyProjection<E, P::Metadata>,
        S::Metadata: Clone + Into<P::Metadata>,
    {
        self.registered.push(RegisteredEvent {
            filter: EventFilter::for_aggregate(E::KIND, A::KIND, aggregate_id.to_string()),
            apply: Box::new(|proj, agg_id, data, metadata, codec| {
                let event: E = codec.deserialize(data)?;
                let metadata_converted: P::Metadata = metadata.clone().into();
                ApplyProjection::apply_projection(proj, agg_id, &event, &metadata_converted);
                Ok(())
            }),
        });
        self
    }

    /// Replays the configured events and materializes the projection.
    ///
    /// Events are dispatched to the projection via the registered handlers,
    /// which deserialize and apply each event through the appropriate
    /// `ApplyProjection` implementation.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError`] when the store fails to load events or when
    /// an event cannot be deserialized.
    pub fn load(self) -> Result<P, ProjectionError<S::Error, <S::Codec as Codec>::Error>>
    where
        S::Metadata: Into<P::Metadata>,
        P::Metadata: From<S::Metadata>,
    {
        let filters: Vec<EventFilter> = self.registered.iter().map(|r| r.filter.clone()).collect();

        let events = self
            .repository
            .store
            .load_events(&filters)
            .map_err(ProjectionError::Store)?;
        let codec = self.repository.store.codec();
        let mut projection = P::default();

        for stored in events {
            let StoredEvent {
                aggregate_id,
                kind,
                data,
                metadata,
                ..
            } = stored;

            // Find the registered handler for this event kind and invoke it
            for reg in &self.registered {
                if reg.filter.event_kind == kind {
                    (reg.apply)(&mut projection, &aggregate_id, &data, &metadata, codec)
                        .map_err(ProjectionError::Codec)?;
                    break;
                }
            }
        }

        Ok(projection)
    }
}

/// Builder for loading aggregates by ID.
pub struct AggregateBuilder<'a, S, A>
where
    S: EventStore,
    A: Aggregate,
{
    repository: &'a Repository<S>,
    _phantom: PhantomData<A>,
}

impl<'a, S, A> AggregateBuilder<'a, S, A>
where
    S: EventStore,
    A: Aggregate,
{
    const fn new(repository: &'a Repository<S>) -> Self {
        Self {
            repository,
            _phantom: PhantomData,
        }
    }

    /// Load the aggregate instance.
    ///
    /// The event kinds to load are automatically determined from the
    /// aggregate's event type via `ProjectionEvent::EVENT_KINDS`.
    ///
    /// # Errors
    ///
    /// Returns an error if the store fails to load events or if events cannot be deserialized.
    pub fn load(
        self,
        id: &A::Id,
    ) -> Result<A, ProjectionError<S::Error, <S::Codec as Codec>::Error>>
    where
        A::Event: ProjectionEvent,
        A::Id: std::fmt::Display,
    {
        // Build filters for all event kinds for this specific aggregate
        let aggregate_id = id.to_string();
        let filters: Vec<EventFilter> = A::Event::EVENT_KINDS
            .iter()
            .map(|kind| EventFilter::for_aggregate(*kind, A::KIND, aggregate_id.clone()))
            .collect();

        let events = self
            .repository
            .store
            .load_events(&filters)
            .map_err(ProjectionError::Store)?;
        let codec = self.repository.store.codec();

        let mut aggregate = A::default();

        for stored in events {
            // Sum type deserializes itself
            let event = A::Event::from_stored(&stored.kind, &stored.data, codec)
                .map_err(ProjectionError::Codec)?;
            aggregate.apply(event);
        }

        Ok(aggregate)
    }
}

/// Errors that can occur when rebuilding a projection.
#[derive(Debug)]
pub enum ProjectionError<StoreError, CodecError> {
    Store(StoreError),
    Codec(CodecError),
}

impl<StoreError, CodecError> fmt::Display for ProjectionError<StoreError, CodecError>
where
    StoreError: fmt::Display,
    CodecError: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "failed to load events: {error}"),
            Self::Codec(error) => write!(f, "failed to decode event: {error}"),
        }
    }
}

impl<StoreError, CodecError> std::error::Error for ProjectionError<StoreError, CodecError>
where
    StoreError: fmt::Debug + fmt::Display + std::error::Error + 'static,
    CodecError: fmt::Debug + fmt::Display + std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Store(error) => Some(error),
            Self::Codec(error) => Some(error),
        }
    }
}

/// Command-side entities that produce domain events.
///
/// Aggregates rebuild their state from events (`Apply<E>`) and validate commands via
/// [`Handle<C>`]. The derive macro generates the event enum and plumbing automatically, while
/// keeping your state struct focused on domain behaviour.
pub trait Aggregate: Default + Sized {
    /// Aggregate type identifier used by the event store.
    ///
    /// This is combined with the aggregate ID to create stream identifiers.
    /// Use lowercase, kebab-case for consistency: `"product"`, `"user-account"`, etc.
    const KIND: &'static str;

    type Event;
    type Error;
    type Id;

    /// Apply an event to update aggregate state.
    ///
    /// This is called during event replay to rebuild aggregate state from history.
    fn apply(&mut self, event: Self::Event);
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
    store: S,
}

impl<S> Repository<S>
where
    S: EventStore,
{
    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn new(store: S) -> Self {
        Self { store }
    }

    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn event_store(&self) -> &S {
        &self.store
    }

    #[allow(clippy::missing_const_for_fn)]
    pub fn event_store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Create a projection builder for flexible query-side loading.
    ///
    /// Use this to build read models by specifying which events to apply
    /// and which streams to load from.
    pub fn build_projection<P>(&self) -> ProjectionBuilder<'_, S, P>
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
        A::Id: std::fmt::Display,
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

/// In-memory event store that keeps streams in a hash map.
///
/// Uses a global sequence counter (`Position = u64`) to maintain chronological
/// ordering across streams, enabling cross-aggregate projections that need to
/// interleave events by time rather than by stream name.
///
/// Generic over metadata type `M`, allowing zero-cost abstraction when metadata
/// is not needed (use `()`) or custom metadata types for specific use cases.
pub struct InMemoryEventStore<C, M = EventMetadata>
where
    C: Codec,
{
    codec: C,
    streams: HashMap<String, Vec<StoredEvent<u64, M>>>,
    next_position: u64,
}

impl<C, M> InMemoryEventStore<C, M>
where
    C: Codec,
{
    #[must_use]
    pub fn new(codec: C) -> Self {
        Self {
            codec,
            streams: HashMap::new(),
            next_position: 0,
        }
    }
}

impl<C, M> EventStore for InMemoryEventStore<C, M>
where
    C: Codec,
    M: Clone,
{
    type Position = u64; // Global sequence for chronological ordering
    type Error = Infallible;
    type Codec = C;
    type Metadata = M;

    fn codec(&self) -> &Self::Codec {
        &self.codec
    }

    fn begin(&mut self, aggregate_kind: &str, aggregate_id: &str) -> Transaction<'_, Self> {
        Transaction::new(self, aggregate_kind.to_string(), aggregate_id.to_string())
    }

    fn append_batch(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &str,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), Self::Error> {
        let stream_key = format!("{aggregate_kind}::{aggregate_id}");
        let stored: Vec<StoredEvent<u64, M>> = events
            .into_iter()
            .map(|e| {
                let position = self.next_position;
                self.next_position += 1;
                StoredEvent {
                    aggregate_kind: aggregate_kind.to_string(),
                    aggregate_id: aggregate_id.to_string(),
                    kind: e.kind,
                    position,
                    data: e.data,
                    metadata: e.metadata,
                }
            })
            .collect();

        self.streams.entry(stream_key).or_default().extend(stored);
        Ok(())
    }

    fn load_events(
        &self,
        filters: &[EventFilter],
    ) -> Result<Vec<StoredEvent<u64, M>>, Self::Error> {
        use std::collections::HashSet;

        let mut result = Vec::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new(); // (aggregate_kind, aggregate_id, event_kind)

        // Group filters by aggregate ID, converting to HashSet for O(1) lookup
        let mut all_kinds: HashSet<String> = HashSet::new(); // Filters with no aggregate restriction
        let mut by_aggregate: HashMap<(String, String), HashSet<String>> = HashMap::new(); // Filters targeting a specific aggregate

        for filter in filters {
            if let (Some(kind), Some(id)) = (&filter.aggregate_kind, &filter.aggregate_id) {
                by_aggregate
                    .entry((kind.clone(), id.clone()))
                    .or_default()
                    .insert(filter.event_kind.clone());
            } else {
                all_kinds.insert(filter.event_kind.clone());
            }
        }

        // Load events for specific aggregates
        for ((aggregate_kind, aggregate_id), kinds) in &by_aggregate {
            let stream_key = format!("{aggregate_kind}::{aggregate_id}");
            if let Some(stream) = self.streams.get(&stream_key) {
                for event in stream.iter().filter(|e| kinds.contains(&e.kind)) {
                    // Track that we've seen this (aggregate_kind, aggregate_id, kind) triple
                    seen.insert((
                        event.aggregate_kind.clone(),
                        event.aggregate_id.clone(),
                        event.kind.clone(),
                    ));
                    result.push(event.clone());
                }
            }
        }

        // Load events from all aggregates for unfiltered kinds
        // Skip events we've already loaded for specific aggregates
        if !all_kinds.is_empty() {
            for stream in self.streams.values() {
                for event in stream.iter().filter(|e| all_kinds.contains(&e.kind)) {
                    let key = (
                        event.aggregate_kind.clone(),
                        event.aggregate_id.clone(),
                        event.kind.clone(),
                    );
                    if !seen.contains(&key) {
                        result.push(event.clone());
                    }
                }
            }
        }

        // Sort by position for chronological ordering across streams
        result.sort_by_key(|event| event.position);

        Ok(result)
    }
}

/// JSON codec backed by `serde_json`.
pub struct JsonCodec;

impl Codec for JsonCodec {
    type Error = serde_json::Error;

    fn serialize<E>(&self, event: &E) -> Result<Vec<u8>, Self::Error>
    where
        E: DomainEvent,
    {
        serde_json::to_vec(event)
    }

    fn deserialize<E>(&self, data: &[u8]) -> Result<E, Self::Error>
    where
        E: DomainEvent,
    {
        serde_json::from_slice(data)
    }
}
