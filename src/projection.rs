//! Read-side primitives.
//!
//! Projections rebuild query models from streams of stored events. This module
//! provides the projection trait, event application hooks via [`ApplyProjection`],
//! and the [`ProjectionBuilder`] that wires everything together.
use std::collections::HashMap;
use std::marker::PhantomData;

use thiserror::Error;

use crate::{
    aggregate::Aggregate,
    codec::Codec,
    event::DomainEvent,
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
/// Implementations receive the aggregate identifier, the pure domain event,
/// and metadata supplied by the backing store.
///
/// ```ignore
/// impl ApplyProjection<String, InventoryAdjusted, ()> for InventoryReport {
///     fn apply_projection(&mut self, aggregate_id: &String, event: &InventoryAdjusted, _metadata: &()) {
///         let stats = self.products.entry(aggregate_id.clone()).or_default();
///         stats.quantity += event.delta;
///     }
/// }
/// ```
pub trait ApplyProjection<Id, E, M> {
    fn apply_projection(&mut self, aggregate_id: &Id, event: &E, metadata: &M);
}

/// Errors that can occur when rebuilding a projection.
#[derive(Debug, Error)]
pub enum ProjectionError<StoreError, CodecError>
where
    StoreError: std::error::Error + 'static,
    CodecError: std::error::Error + 'static,
{
    #[error("failed to load events: {0}")]
    Store(#[source] StoreError),
    #[error("failed to decode event kind `{event_kind}`: {error}")]
    Codec {
        event_kind: String,
        #[source]
        error: CodecError,
    },
    #[error("failed to decode event: {0}")]
    EventDecode(#[source] crate::codec::EventDecodeError<CodecError>),
    #[error("failed to deserialize snapshot: {0}")]
    SnapshotDeserialize(#[source] CodecError),
}

/// Type alias for event handler closures.
type EventHandler<P, S> = Box<
    dyn Fn(
        &mut P,
        &<S as EventStore>::Id,
        &[u8],
        &<S as EventStore>::Metadata,
        &<S as EventStore>::Codec,
    ) -> Result<(), <<S as EventStore>::Codec as Codec>::Error>,
>;

/// Builder used to configure which events should be loaded for a projection.
pub struct ProjectionBuilder<'a, S, P>
where
    S: EventStore,
    P: Projection,
{
    pub(super) store: &'a S,
    /// Event kind -> handler mapping for O(1) dispatch
    handlers: HashMap<String, EventHandler<P, S>>,
    /// Filters for loading events from the store
    filters: Vec<EventFilter<S::Id, S::Position>>,
    pub(super) _phantom: PhantomData<P>,
}

impl<'a, S, P> ProjectionBuilder<'a, S, P>
where
    S: EventStore,
    P: Projection,
{
    pub(super) fn new(store: &'a S) -> Self {
        Self {
            store,
            handlers: HashMap::new(),
            filters: Vec::new(),
            _phantom: PhantomData,
        }
    }

    /// Register a specific event type to load from all aggregates.
    ///
    /// # Type Constraints
    ///
    /// The store's metadata type must be convertible to the projection's metadata type.
    /// `Clone` is required because event handlers receive metadata by reference, but
    /// `Into::into()` requires ownership. The metadata is cloned once per event.
    ///
    /// # Example
    /// ```ignore
    /// builder.event::<ProductRestocked>()  // All products
    /// ```
    #[must_use]
    pub fn event<E>(mut self) -> Self
    where
        E: DomainEvent,
        P: ApplyProjection<S::Id, E, P::Metadata>,
        S::Metadata: Clone + Into<P::Metadata>,
    {
        self.filters.push(EventFilter::for_event(E::KIND));
        self.handlers.insert(
            E::KIND.to_string(),
            Box::new(|proj, agg_id, data, metadata, codec| {
                let event: E = codec.deserialize(data)?;
                let metadata_converted: P::Metadata = metadata.clone().into();
                ApplyProjection::apply_projection(proj, agg_id, &event, &metadata_converted);
                Ok(())
            }),
        );
        self
    }

    /// Register a specific event type to load from a specific aggregate instance.
    ///
    /// # Example
    /// ```ignore
    /// builder.event_for::<Account, FundsDeposited>(&account_id); // One account stream
    /// ```
    #[must_use]
    pub fn event_for<A, E>(mut self, aggregate_id: &S::Id) -> Self
    where
        A: Aggregate<Id = S::Id>,
        E: DomainEvent,
        P: ApplyProjection<S::Id, E, P::Metadata>,
        S::Metadata: Clone + Into<P::Metadata>,
    {
        self.filters.push(EventFilter::for_aggregate(
            E::KIND,
            A::KIND,
            aggregate_id.clone(),
        ));
        self.handlers.insert(
            E::KIND.to_string(),
            Box::new(|proj, agg_id, data, metadata, codec| {
                let event: E = codec.deserialize(data)?;
                let metadata_converted: P::Metadata = metadata.clone().into();
                ApplyProjection::apply_projection(proj, agg_id, &event, &metadata_converted);
                Ok(())
            }),
        );
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
    #[tracing::instrument(
        skip(self),
        fields(
            projection_type = std::any::type_name::<P>(),
            filter_count = self.filters.len(),
            handler_count = self.handlers.len()
        )
    )]
    pub fn load(self) -> Result<P, ProjectionError<S::Error, <S::Codec as Codec>::Error>> {
        tracing::debug!("loading projection");

        let events = self
            .store
            .load_events(&self.filters)
            .map_err(ProjectionError::Store)?;
        let codec = self.store.codec();
        let mut projection = P::default();

        let event_count = events.len();
        tracing::debug!(
            events_to_replay = event_count,
            "replaying events into projection"
        );

        for stored in events {
            let StoredEvent {
                aggregate_id,
                kind,
                data,
                metadata,
                ..
            } = stored;

            // O(1) handler lookup instead of O(n) linear scan
            if let Some(handler) = self.handlers.get(&kind) {
                (handler)(&mut projection, &aggregate_id, &data, &metadata, codec).map_err(
                    |error| ProjectionError::Codec {
                        event_kind: kind.clone(),
                        error,
                    },
                )?;
            }
            // Unknown kinds are intentionally skipped - projections only care about
            // the events they explicitly registered handlers for
        }

        tracing::info!(events_applied = event_count, "projection loaded");
        Ok(projection)
    }
}
