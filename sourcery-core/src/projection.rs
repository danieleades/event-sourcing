//! Read-side primitives.
//!
//! Projections rebuild query models from streams of stored events. This module
//! provides the projection trait, event application hooks via
//! [`ApplyProjection`], and the [`ProjectionBuilder`] that wires everything
//! together.
use std::{collections::HashMap, marker::PhantomData};

use thiserror::Error;

use crate::{
    aggregate::Aggregate,
    codec::{Codec, EventDecodeError, ProjectionEvent},
    event::DomainEvent,
    store::{EventFilter, EventStore, StoredEvent},
};

/// Trait implemented by read models that can be constructed from an event
/// stream.
///
/// Implementors specify the identifier and metadata types their
/// [`ApplyProjection`] handlers expect. Projections are typically rebuilt by
/// calling [`Repository::build_projection`] and configuring the desired event
/// streams before invoking [`ProjectionBuilder::load`].
// ANCHOR: projection_trait
pub trait Projection: Default + Sized {
    /// Aggregate identifier type this projection is compatible with.
    type Id;
    /// Metadata type expected by this projection
    type Metadata;
}
// ANCHOR_END: projection_trait

/// Apply an event to a projection with access to envelope context.
///
/// Implementations receive the aggregate identifier, the pure domain event,
/// and metadata supplied by the backing store.
///
/// ```ignore
/// impl ApplyProjection<InventoryAdjusted> for InventoryReport {
///     fn apply_projection(&mut self, aggregate_id: &Self::Id, event: &InventoryAdjusted, _metadata: &Self::Metadata) {
///         let stats = self.products.entry(aggregate_id.clone()).or_default();
///         stats.quantity += event.delta;
///     }
/// }
/// ```
// ANCHOR: apply_projection_trait
pub trait ApplyProjection<E>: Projection {
    fn apply_projection(&mut self, aggregate_id: &Self::Id, event: &E, metadata: &Self::Metadata);
}
// ANCHOR_END: apply_projection_trait

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
#[derive(Debug)]
enum HandlerError<CodecError> {
    Codec(CodecError),
    EventDecode(EventDecodeError<CodecError>),
}

impl<CodecError> From<CodecError> for HandlerError<CodecError> {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}

type EventHandler<P, S> = Box<
    dyn Fn(
            &mut P,
            &<S as EventStore>::Id,
            &[u8],
            &<S as EventStore>::Metadata,
            &<S as EventStore>::Codec,
        ) -> Result<(), HandlerError<<<S as EventStore>::Codec as Codec>::Error>>
        + Send
        + Sync,
>;

/// Builder used to configure which events should be loaded for a projection.
pub struct ProjectionBuilder<'a, S, P>
where
    S: EventStore,
    P: Projection<Id = S::Id>,
{
    pub(super) store: &'a S,
    /// Event kind -> handler mapping for O(1) dispatch
    handlers: HashMap<String, EventHandler<P, S>>,
    /// Filters for loading events from the store
    filters: Vec<EventFilter<S::Id, S::Position>>,
    pub(super) _phantom: PhantomData<fn() -> P>,
}

impl<'a, S, P> ProjectionBuilder<'a, S, P>
where
    S: EventStore,
    P: Projection<Id = S::Id>,
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
    /// The store's metadata type must be convertible to the projection's
    /// metadata type. `Clone` is required because event handlers receive
    /// metadata by reference, but `Into::into()` requires ownership. The
    /// metadata is cloned once per event.
    ///
    /// # Example
    /// ```ignore
    /// builder.event::<ProductRestocked>()  // All products
    /// ```
    #[must_use]
    pub fn event<E>(mut self) -> Self
    where
        E: DomainEvent,
        P: ApplyProjection<E>,
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

    /// Register all event kinds supported by a `ProjectionEvent` sum type
    /// across all aggregates.
    ///
    /// This is primarily intended for subscribing to an aggregate's generated
    /// event enum (`A::Event` from `#[derive(Aggregate)]`) as a single
    /// "unit", rather than registering each `DomainEvent` type
    /// individually.
    ///
    /// # Example
    /// ```ignore
    /// builder.events::<AccountEvent>() // All accounts, all account event variants
    /// ```
    #[must_use]
    pub fn events<E>(mut self) -> Self
    where
        E: ProjectionEvent,
        P: ApplyProjection<E>,
        S::Metadata: Clone + Into<P::Metadata>,
    {
        for &kind in E::EVENT_KINDS {
            self.filters.push(EventFilter::for_event(kind));
            self.handlers.insert(
                kind.to_string(),
                Box::new(move |proj, agg_id, data, metadata, codec| {
                    let event =
                        E::from_stored(kind, data, codec).map_err(HandlerError::EventDecode)?;
                    let metadata_converted: P::Metadata = metadata.clone().into();
                    ApplyProjection::apply_projection(proj, agg_id, &event, &metadata_converted);
                    Ok(())
                }),
            );
        }
        self
    }

    /// Register a specific event type to load from a specific aggregate
    /// instance.
    ///
    /// Use this when you only care about a single event kind. If you want to
    /// subscribe to an aggregate's full event enum (`A::Event`), prefer
    /// [`ProjectionBuilder::events_for`].
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
        P: ApplyProjection<E>,
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

    /// Register all event kinds for a specific aggregate instance.
    ///
    /// This subscribes the projection to the aggregate's event sum type
    /// (`A::Event`) and loads all events in that stream that correspond to
    /// `A::Event::EVENT_KINDS`.
    ///
    /// # Example
    /// ```ignore
    /// let history = repository
    ///     .build_projection::<AccountHistory>()
    ///     .events_for::<Account>(&account_id)
    ///     .load()?;
    /// ```
    #[must_use]
    pub fn events_for<A>(mut self, aggregate_id: &S::Id) -> Self
    where
        A: Aggregate<Id = S::Id>,
        A::Event: ProjectionEvent,
        P: ApplyProjection<A::Event>,
        S::Metadata: Clone + Into<P::Metadata>,
    {
        for &kind in <A::Event as ProjectionEvent>::EVENT_KINDS {
            self.filters.push(EventFilter::for_aggregate(
                kind,
                A::KIND,
                aggregate_id.clone(),
            ));
            self.handlers.insert(
                kind.to_string(),
                Box::new(move |proj, agg_id, data, metadata, codec| {
                    let event = <A::Event as ProjectionEvent>::from_stored(kind, data, codec)
                        .map_err(HandlerError::EventDecode)?;
                    let metadata_converted: P::Metadata = metadata.clone().into();
                    ApplyProjection::apply_projection(proj, agg_id, &event, &metadata_converted);
                    Ok(())
                }),
            );
        }
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
    pub async fn load(self) -> Result<P, ProjectionError<S::Error, <S::Codec as Codec>::Error>> {
        tracing::debug!("loading projection");

        let events = self
            .store
            .load_events(&self.filters)
            .await
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
                    |error| match error {
                        HandlerError::Codec(error) => ProjectionError::Codec {
                            event_kind: kind.clone(),
                            error,
                        },
                        HandlerError::EventDecode(error) => ProjectionError::EventDecode(error),
                    },
                )?;
            }
            // Unknown kinds are intentionally skipped - projections only care
            // about the events they explicitly registered handlers
            // for
        }

        tracing::info!(events_applied = event_count, "projection loaded");
        Ok(projection)
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, io};

    use super::*;

    #[test]
    fn projection_error_display_store_mentions_loading() {
        let error: ProjectionError<io::Error, io::Error> =
            ProjectionError::Store(io::Error::new(io::ErrorKind::NotFound, "not found"));
        let msg = error.to_string();
        assert!(msg.contains("failed to load events"));
        assert!(error.source().is_some());
    }

    #[test]
    fn projection_error_display_codec_includes_event_kind() {
        let error: ProjectionError<io::Error, io::Error> = ProjectionError::Codec {
            event_kind: "test-event".to_string(),
            error: io::Error::new(io::ErrorKind::InvalidData, "bad data"),
        };
        let msg = error.to_string();
        assert!(msg.contains("failed to decode event kind `test-event`"));
        assert!(error.source().is_some());
    }
}
