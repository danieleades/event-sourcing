//! Read-side primitives.
//!
//! Projections rebuild query models from streams of stored events. This module
//! provides the projection trait, event application hooks, the optional
//! `EventEnvelope`, and the `ProjectionBuilder` that wires everything together.
use std::{fmt, marker::PhantomData};

use crate::{
    aggregate::Aggregate,
    codec::Codec,
    event::DomainEvent,
    repository::Repository,
    snapshot::SnapshotStore,
    store::{EventFilter, EventStore, StoredEvent},
};

/// Trait implemented by read models that can be constructed from an event stream.
///
/// Implementors specify the metadata type their [`ApplyProjection`] handlers expect.
/// Projections are typically rebuilt by calling [`Repository::build_projection`] and
/// configuring the desired event streams before invoking [`ProjectionBuilder::load`].
pub trait Projection: Default + Sized {
    /// Metadata type expected by this projection
    type Metadata;
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
pub trait ApplyProjection<E, M> {
    fn apply_projection(&mut self, aggregate_id: &str, event: &E, metadata: &M);
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
pub struct ProjectionBuilder<'a, S, SS, P>
where
    S: EventStore,
    SS: SnapshotStore,
    P: Projection,
{
    pub(super) repository: &'a Repository<S, SS>,
    registered: Vec<RegisteredEvent<P, S>>,
    pub(super) _phantom: PhantomData<P>,
}

impl<'a, S, SS, P> ProjectionBuilder<'a, S, SS, P>
where
    S: EventStore,
    SS: SnapshotStore,
    P: Projection,
{
    pub(super) const fn new(repository: &'a Repository<S, SS>) -> Self {
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
