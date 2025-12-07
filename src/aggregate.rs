//! Command-side domain primitives.
//!
//! This module defines the building blocks for aggregates: state reconstruction
//! (`Apply`), command handling (`Handle`), and loading (`AggregateBuilder`).
//! The `#[derive(Aggregate)]` macro lives here to keep domain ergonomics in one spot.
// Re-export the derive macros so users only need one import
pub use event_sourcing_macros::Aggregate;

use std::{fmt, marker::PhantomData};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    codec::{Codec, ProjectionEvent},
    projection::ProjectionError,
    repository::Repository,
    snapshot::SnapshotStore,
    store::{EventFilter, EventStore},
};

/// Command-side entities that produce domain events.
///
/// Aggregates rebuild their state from events (`Apply<E>`) and validate commands via
/// [`Handle<C>`]. The derive macro generates the event enum and plumbing automatically, while
/// keeping your state struct focused on domain behaviour.
///
/// Aggregates must be serializable to support snapshotting. Use `#[derive(Serialize, Deserialize)]`
/// or implement the traits manually. Schema evolution can be handled using `serde_evolve::Versioned`.
pub trait Aggregate: Default + Sized + Serialize + DeserializeOwned {
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
    ///
    /// When using `#[derive(Aggregate)]`, this dispatches to your `Apply<E>` implementations.
    /// For hand-written aggregates, implement this directly with a match expression.
    fn apply(&mut self, event: &Self::Event);
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

/// Builder for loading aggregates by ID.
pub struct AggregateBuilder<'a, S, SS, A>
where
    S: EventStore,
    SS: SnapshotStore,
    A: Aggregate,
{
    pub(super) repository: &'a Repository<S, SS>,
    pub(super) _phantom: PhantomData<A>,
}

impl<'a, S, SS, A> AggregateBuilder<'a, S, SS, A>
where
    S: EventStore,
    SS: SnapshotStore,
    A: Aggregate,
{
    pub(super) const fn new(repository: &'a Repository<S, SS>) -> Self {
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
        A::Id: fmt::Display,
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
            aggregate.apply(&event);
        }

        Ok(aggregate)
    }
}
