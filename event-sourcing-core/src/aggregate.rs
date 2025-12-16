//! Command-side domain primitives.
//!
//! This module defines the building blocks for aggregates: state reconstruction
//! (`Apply`), command handling (`Handle`), and loading (`AggregateBuilder`).
//! The `#[derive(Aggregate)]` macro lives here to keep domain ergonomics in one spot.

use std::marker::PhantomData;

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    codec::{Codec, ProjectionEvent},
    concurrency::ConcurrencyStrategy,
    projection::ProjectionError,
    repository::{Repository, Snapshots},
    snapshot::{NoSnapshots, SnapshotStore},
    store::EventStore,
};

/// Command-side entities that produce domain events.
///
/// Aggregates rebuild their state from events (`Apply<E>`) and validate commands via
/// [`Handle<C>`]. The derive macro generates the event enum and plumbing automatically, while
/// keeping your state struct focused on domain behaviour.
///
/// Aggregates are domain objects and do not require serialization by default.
///
/// If you enable snapshots (via `Repository::with_snapshots`), the aggregate state must be
/// serializable (`Serialize + DeserializeOwned`).
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
pub struct AggregateBuilder<'a, R, A> {
    pub(super) repository: &'a R,
    pub(super) _phantom: PhantomData<fn() -> A>,
}

impl<'a, R, A> AggregateBuilder<'a, R, A> {
    pub(super) const fn new(repository: &'a R) -> Self {
        Self {
            repository,
            _phantom: PhantomData,
        }
    }
}

impl<S, C, A> AggregateBuilder<'_, Repository<S, C, NoSnapshots<S::Id, S::Position>>, A>
where
    S: EventStore,
    C: ConcurrencyStrategy,
    A: Aggregate<Id = S::Id>,
{
    /// Load the aggregate instance by replaying events (no snapshots).
    ///
    /// The event kinds to load are automatically determined from the
    /// aggregate's event type via `ProjectionEvent::EVENT_KINDS`.
    ///
    /// # Errors
    ///
    /// Returns an error if the store fails to load events or if events cannot be deserialized.
    pub async fn load(
        self,
        id: &S::Id,
    ) -> Result<A, ProjectionError<S::Error, <S::Codec as Codec>::Error>>
    where
        A::Event: ProjectionEvent,
    {
        self.repository.load::<A>(id).await
    }
}

impl<S, SS, C, A> AggregateBuilder<'_, Repository<S, C, Snapshots<SS>>, A>
where
    S: EventStore,
    SS: SnapshotStore<Id = S::Id, Position = S::Position>,
    C: ConcurrencyStrategy,
    A: Aggregate<Id = S::Id> + Serialize + DeserializeOwned,
{
    /// Load the aggregate instance using snapshots when available.
    ///
    /// The event kinds to load are automatically determined from the
    /// aggregate's event type via `ProjectionEvent::EVENT_KINDS`.
    ///
    /// # Errors
    ///
    /// Returns an error if events cannot be deserialized or a stored snapshot cannot
    /// be deserialized (which indicates snapshot data corruption).
    pub async fn load(
        self,
        id: &S::Id,
    ) -> Result<A, ProjectionError<S::Error, <S::Codec as Codec>::Error>>
    where
        A::Event: ProjectionEvent,
    {
        self.repository.load::<A>(id).await
    }
}
