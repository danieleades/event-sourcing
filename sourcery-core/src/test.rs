//! Test utilities for event-sourced aggregates.
//!
//! This module provides testing utilities for event-sourced systems:
//!
//! - [`TestFramework`]: BDD-style unit testing for aggregates in isolation
//! - [`RepositoryTestExt`]: Extension trait for integration testing with real repositories
//!
//! # Unit Testing with [`TestFramework`]
//!
//! The [`TestFramework`] is inspired by [cqrs-es](https://crates.io/crates/cqrs-es)
//! for testing aggregate behavior in isolation, without requiring a real event store.
//!
//! ```ignore
//! use sourcery::test::TestFramework;
//!
//! type CounterTest = TestFramework<Counter>;
//!
//! #[test]
//! fn adding_value_produces_event() {
//!     CounterTest::new()
//!         .given_no_previous_events()
//!         .when(&AddValue { amount: 10 })
//!         .then_expect_events(&[
//!             CounterEvent::Added(ValueAdded { amount: 10 })
//!         ]);
//! }
//!
//! #[test]
//! fn cannot_subtract_more_than_balance() {
//!     CounterTest::new()
//!         .given(vec![
//!             CounterEvent::Added(ValueAdded { amount: 10 })
//!         ])
//!         .when(&SubtractValue { amount: 20 })
//!         .then_expect_error_message("insufficient value");
//! }
//! ```
//!
//! # Integration Testing with [`RepositoryTestExt`]
//!
//! For integration tests that need a real repository, use [`RepositoryTestExt`]:
//!
//! ```ignore
//! use sourcery::test::RepositoryTestExt;
//!
//! // Seed initial events (e.g., for projection tests)
//! repo.seed_events::<Product>(&product_id, vec![
//!     ProductRestocked { sku: "SKU-001".into(), quantity: 100 }.into(),
//! ])?;
//!
//! // Simulate concurrent writes (e.g., for optimistic concurrency tests)
//! repo.inject_concurrent_event::<InventoryItem>(
//!     &item_id,
//!     ItemReserved { quantity: 20 }.into(),
//! )?;
//! ```

use std::fmt;
use std::future::Future;
use std::marker::PhantomData;

use thiserror::Error;

use crate::aggregate::{Aggregate, Handle};
use crate::codec::{Codec, SerializableEvent};
use crate::concurrency::{ConcurrencyStrategy, Unchecked};
use crate::repository::Repository;
use crate::store::{AppendError, EventStore, PersistableEvent};

// =============================================================================
// Repository Test Extension Trait
// =============================================================================

/// Error type for seeding operations.
#[derive(Debug, Error)]
pub enum SeedError<StoreError, CodecError>
where
    StoreError: std::error::Error + 'static,
    CodecError: std::error::Error + 'static,
{
    /// Failed to serialize an event.
    #[error("failed to serialize event: {0}")]
    Codec(#[source] CodecError),
    /// Failed to persist events to the store.
    #[error("failed to persist event: {0}")]
    Store(#[source] StoreError),
}

type SeedResult<S> =
    Result<(), SeedError<<S as EventStore>::Error, <<S as EventStore>::Codec as Codec>::Error>>;

/// Extension trait providing test utilities for [`Repository`].
///
/// These methods are designed for testing scenarios where you need to:
/// - Seed initial event history before testing commands or projections
/// - Simulate concurrent modifications from other processes
///
/// All methods bypass the normal command handling flow, allowing you to
/// set up test fixtures without going through aggregate business logic.
///
/// # Example
///
/// ```ignore
/// use sourcery::test::RepositoryTestExt;
///
/// // Seed initial state for a projection test
/// repo.seed_events::<Product>(&product_id, vec![
///     ProductRestocked { sku: "SKU-001".into(), quantity: 100 }.into(),
/// ])?;
///
/// // Simulate a concurrent write for optimistic concurrency testing
/// repo.inject_concurrent_event::<Product>(
///     &product_id,
///     ProductEvent::from(ProductRestocked { sku: "SKU-001".into(), quantity: 50 }),
/// )?;
/// ```
pub trait StoreAccess {
    type Store: EventStore;

    fn store(&self) -> &Self::Store;
    fn store_mut(&mut self) -> &mut Self::Store;
}

impl<S, C, M> StoreAccess for Repository<S, C, M>
where
    S: EventStore,
    C: ConcurrencyStrategy,
{
    type Store = S;

    fn store(&self) -> &Self::Store {
        &self.store
    }

    fn store_mut(&mut self) -> &mut Self::Store {
        &mut self.store
    }
}

pub trait RepositoryTestExt: StoreAccess + Send {
    /// Seed events for an aggregate, bypassing command handlers.
    ///
    /// Events are serialized through the codec, ensuring they can be loaded
    /// correctly. This is useful for setting up test fixtures without
    /// executing commands.
    ///
    /// # Arguments
    ///
    /// * `id` - The aggregate instance identifier
    /// * `events` - Events to append (must implement [`SerializableEvent`])
    ///
    /// # Errors
    ///
    /// Returns [`SeedError::Codec`] if event serialization fails, or
    /// [`SeedError::Store`] if persistence fails.
    fn seed_events<'a, A>(
        &'a mut self,
        id: &<Self::Store as EventStore>::Id,
        events: Vec<A::Event>,
    ) -> impl Future<Output = SeedResult<Self::Store>> + Send + 'a
    where
        A: Aggregate<Id = <Self::Store as EventStore>::Id>,
        A::Event: SerializableEvent + Send + 'a,
        <Self::Store as EventStore>::Metadata: Default,
    {
        let id = id.clone();
        async move {
            let store = self.store_mut();
            let mut tx = store.begin::<Unchecked>(A::KIND, id, None);

            for event in events {
                tx.append(event, <Self::Store as EventStore>::Metadata::default())
                    .map_err(SeedError::Codec)?;
            }

            tx.commit().await.map_err(SeedError::Store)
        }
    }

    /// Inject a single event as if from a concurrent writer.
    ///
    /// This simulates what happens when another process appends events
    /// to the same aggregate stream. The event goes through the codec
    /// for proper serialization.
    ///
    /// Use this to test optimistic concurrency conflict detection.
    ///
    /// # Arguments
    ///
    /// * `id` - The aggregate instance identifier
    /// * `event` - The event to inject
    ///
    /// # Errors
    ///
    /// Returns [`SeedError::Codec`] if serialization fails, or
    /// [`SeedError::Store`] if persistence fails.
    fn inject_concurrent_event<'a, A>(
        &'a mut self,
        id: &<Self::Store as EventStore>::Id,
        event: A::Event,
    ) -> impl Future<Output = SeedResult<Self::Store>> + Send + 'a
    where
        A: Aggregate<Id = <Self::Store as EventStore>::Id>,
        A::Event: SerializableEvent + Send + 'a,
        <Self::Store as EventStore>::Metadata: Default,
    {
        let id = id.clone();
        async move {
            let store = self.store_mut();
            let persistable = event
                .to_persistable(
                    store.codec(),
                    <Self::Store as EventStore>::Metadata::default(),
                )
                .map_err(SeedError::Codec)?;

            store
                .append(A::KIND, &id, None, vec![persistable])
                .await
                .map_err(|e| match e {
                    AppendError::Store(e) => SeedError::Store(e),
                    AppendError::Conflict(_) => {
                        unreachable!("no version check on inject_concurrent_event")
                    }
                })
        }
    }

    /// Inject a raw event without codec processing.
    ///
    /// This is the lowest-level test injection, allowing complete control
    /// over the serialized form. Use this only when you need to test
    /// specific wire formats or malformed data handling.
    ///
    /// # Arguments
    ///
    /// * `aggregate_kind` - The aggregate type identifier (e.g., `Aggregate::KIND`)
    /// * `id` - The aggregate instance identifier
    /// * `event` - Raw persistable event with pre-serialized data
    ///
    /// # Errors
    ///
    /// Returns a store error if persistence fails.
    fn inject_raw_event(
        &mut self,
        aggregate_kind: &str,
        id: &<Self::Store as EventStore>::Id,
        event: PersistableEvent<<Self::Store as EventStore>::Metadata>,
    ) -> impl Future<Output = Result<(), <Self::Store as EventStore>::Error>> + Send + '_ {
        let aggregate_kind = aggregate_kind.to_string();
        let id = id.clone();
        async move {
            let store = self.store_mut();
            store
                .append(&aggregate_kind, &id, None, vec![event])
                .await
                .map_err(|e| match e {
                    AppendError::Store(e) => e,
                    AppendError::Conflict(_) => {
                        unreachable!("no version check on inject_raw_event")
                    }
                })
        }
    }
}

impl<T> RepositoryTestExt for T where T: StoreAccess + Send {}

// =============================================================================
// Test Framework for Aggregate Unit Testing
// =============================================================================

/// Test framework for aggregate testing using a given-when-then pattern.
///
/// This framework allows testing aggregate behavior without persistence,
/// focusing on the pure command handling logic.
///
/// # Type Parameters
///
/// * `A` - The aggregate type being tested
pub struct TestFramework<A: Aggregate> {
    _phantom: PhantomData<A>,
}

impl<A: Aggregate> Default for TestFramework<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Aggregate> TestFramework<A> {
    /// Create a new test framework instance.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }

    /// Start a test scenario with no previous events.
    ///
    /// The aggregate will be initialized with its default state.
    #[must_use]
    pub fn given_no_previous_events(self) -> TestExecutor<A> {
        TestExecutor {
            aggregate: A::default(),
        }
    }

    /// Start a test scenario with previous events already applied.
    ///
    /// The events are applied in order to rebuild the aggregate state
    /// before the command is executed.
    #[must_use]
    pub fn given(self, events: &[A::Event]) -> TestExecutor<A> {
        let mut aggregate = A::default();
        for event in events {
            aggregate.apply(event);
        }
        TestExecutor { aggregate }
    }
}

/// Executor that holds the aggregate state and waits for a command.
pub struct TestExecutor<A: Aggregate> {
    aggregate: A,
}

impl<A: Aggregate> TestExecutor<A> {
    /// Execute a command against the aggregate.
    ///
    /// Returns a `TestResult` that can be used to verify the outcome.
    #[must_use]
    pub fn when<C>(self, command: &C) -> TestResult<A>
    where
        A: Handle<C>,
    {
        let result = self.aggregate.handle(command);
        TestResult { result }
    }

    /// Add more events to the aggregate state before executing the command.
    ///
    /// Useful for building up complex state in multiple steps.
    #[must_use]
    pub fn and(mut self, events: Vec<A::Event>) -> Self {
        for event in events {
            self.aggregate.apply(&event);
        }
        self
    }
}

/// Result of executing a command, ready for assertions.
pub struct TestResult<A: Aggregate> {
    result: Result<Vec<A::Event>, A::Error>,
}

impl<A: Aggregate> TestResult<A> {
    /// Assert that the command produced exactly the expected events.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The command returned an error
    /// - The events don't match the expected events
    #[track_caller]
    pub fn then_expect_events(self, expected: &[A::Event])
    where
        A::Event: PartialEq + fmt::Debug,
        A::Error: fmt::Debug,
    {
        match self.result {
            Ok(events) => {
                assert_eq!(
                    events, expected,
                    "Expected events did not match actual events"
                );
            }
            Err(error) => {
                panic!("Expected events but got error: {error:?}");
            }
        }
    }

    /// Assert that the command produced no events.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The command returned an error
    /// - The command produced any events
    #[track_caller]
    pub fn then_expect_no_events(self)
    where
        A::Event: fmt::Debug,
        A::Error: fmt::Debug,
    {
        match self.result {
            Ok(events) => {
                assert!(events.is_empty(), "Expected no events but got: {events:?}");
            }
            Err(error) => {
                panic!("Expected no events but got error: {error:?}");
            }
        }
    }

    /// Assert that the command returned an error.
    ///
    /// # Panics
    ///
    /// Panics if the command succeeded.
    #[track_caller]
    pub fn then_expect_error(self)
    where
        A::Event: fmt::Debug,
    {
        if let Ok(events) = self.result {
            panic!("Expected error but got events: {events:?}");
        }
    }

    /// Assert that the command returned a specific error.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The command succeeded
    /// - The error doesn't match the expected error
    #[track_caller]
    pub fn then_expect_error_eq(self, expected: &A::Error)
    where
        A::Event: fmt::Debug,
        A::Error: PartialEq + fmt::Debug,
    {
        match self.result {
            Ok(events) => {
                panic!("Expected error but got events: {events:?}");
            }
            Err(error) => {
                assert_eq!(
                    error, *expected,
                    "Expected error did not match actual error"
                );
            }
        }
    }

    /// Assert that the command returned an error containing the given message.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The command succeeded
    /// - The error message doesn't contain the expected substring
    #[track_caller]
    pub fn then_expect_error_message(self, expected_substring: &str)
    where
        A::Event: fmt::Debug,
        A::Error: fmt::Display,
    {
        match self.result {
            Ok(events) => {
                panic!("Expected error but got events: {events:?}");
            }
            Err(error) => {
                let error_msg = error.to_string();
                assert!(
                    error_msg.contains(expected_substring),
                    "Expected error message to contain '{expected_substring}' but got: {error_msg}"
                );
            }
        }
    }

    /// Get the raw result for custom assertions.
    ///
    /// This is useful when you need more complex validation logic
    /// that isn't covered by the built-in assertion methods.
    ///
    /// # Errors
    ///
    /// Returns any command handling error produced by the aggregate.
    pub fn inspect_result(self) -> Result<Vec<A::Event>, A::Error> {
        self.result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    // Test fixtures
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct ValueAdded {
        amount: i32,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct ValueSubtracted {
        amount: i32,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum CounterEvent {
        Added(ValueAdded),
        Subtracted(ValueSubtracted),
    }

    #[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
    struct Counter {
        value: i32,
    }

    impl Aggregate for Counter {
        const KIND: &'static str = "counter";
        type Event = CounterEvent;
        type Error = String;
        type Id = String;

        fn apply(&mut self, event: &Self::Event) {
            match event {
                CounterEvent::Added(e) => self.value += e.amount,
                CounterEvent::Subtracted(e) => self.value -= e.amount,
            }
        }
    }

    struct AddValue {
        amount: i32,
    }

    struct SubtractValue {
        amount: i32,
    }

    impl Handle<AddValue> for Counter {
        fn handle(&self, command: &AddValue) -> Result<Vec<Self::Event>, Self::Error> {
            if command.amount <= 0 {
                return Err("amount must be positive".to_string());
            }
            Ok(vec![CounterEvent::Added(ValueAdded {
                amount: command.amount,
            })])
        }
    }

    impl Handle<SubtractValue> for Counter {
        fn handle(&self, command: &SubtractValue) -> Result<Vec<Self::Event>, Self::Error> {
            if command.amount <= 0 {
                return Err("amount must be positive".to_string());
            }
            if self.value < command.amount {
                return Err("insufficient value".to_string());
            }
            Ok(vec![CounterEvent::Subtracted(ValueSubtracted {
                amount: command.amount,
            })])
        }
    }

    type CounterTest = TestFramework<Counter>;

    #[test]
    fn given_no_events_when_add_then_produces_event() {
        CounterTest::new()
            .given_no_previous_events()
            .when(&AddValue { amount: 10 })
            .then_expect_events(&[CounterEvent::Added(ValueAdded { amount: 10 })]);
    }

    #[test]
    fn given_events_when_subtract_then_produces_event() {
        CounterTest::new()
            .given(&[CounterEvent::Added(ValueAdded { amount: 20 })])
            .when(&SubtractValue { amount: 5 })
            .then_expect_events(&[CounterEvent::Subtracted(ValueSubtracted { amount: 5 })]);
    }

    #[test]
    fn given_insufficient_balance_when_subtract_then_error() {
        CounterTest::new()
            .given(&[CounterEvent::Added(ValueAdded { amount: 10 })])
            .when(&SubtractValue { amount: 20 })
            .then_expect_error();
    }

    #[test]
    fn given_insufficient_balance_when_subtract_then_error_message() {
        CounterTest::new()
            .given(&[CounterEvent::Added(ValueAdded { amount: 10 })])
            .when(&SubtractValue { amount: 20 })
            .then_expect_error_message("insufficient value");
    }

    #[test]
    fn given_events_and_more_events_when_command() {
        CounterTest::new()
            .given(&[CounterEvent::Added(ValueAdded { amount: 10 })])
            .and(vec![CounterEvent::Added(ValueAdded { amount: 5 })])
            .when(&SubtractValue { amount: 12 })
            .then_expect_events(&[CounterEvent::Subtracted(ValueSubtracted { amount: 12 })]);
    }

    #[test]
    fn invalid_command_returns_error() {
        CounterTest::new()
            .given_no_previous_events()
            .when(&AddValue { amount: -5 })
            .then_expect_error_message("amount must be positive");
    }

    #[test]
    fn inspect_result_returns_raw_result() {
        let result = CounterTest::new()
            .given_no_previous_events()
            .when(&AddValue { amount: 10 })
            .inspect_result();

        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }

    #[test]
    fn default_creates_new_framework() {
        let _framework: TestFramework<Counter> = TestFramework::default();
    }

    struct NoOp;

    impl Handle<NoOp> for Counter {
        fn handle(&self, _: &NoOp) -> Result<Vec<Self::Event>, Self::Error> {
            Ok(vec![])
        }
    }

    #[test]
    fn no_op_command_produces_no_events() {
        CounterTest::new()
            .given_no_previous_events()
            .when(&NoOp)
            .then_expect_no_events();
    }

    #[test]
    fn then_expect_error_eq_matches_error_value() {
        CounterTest::new()
            .given_no_previous_events()
            .when(&AddValue { amount: -5 })
            .then_expect_error_eq(&"amount must be positive".to_string());
    }
}

#[cfg(test)]
mod repository_test_ext_tests {
    use super::*;
    use crate::{
        codec::{EventDecodeError, ProjectionEvent},
        event::DomainEvent,
        store::{JsonCodec, PersistableEvent, inmemory},
    };

    // Test fixtures with SerializableEvent implementation
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct PointsAdded {
        points: i32,
    }

    impl DomainEvent for PointsAdded {
        const KIND: &'static str = "points-added";
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum ScoreEvent {
        Added(PointsAdded),
    }

    impl SerializableEvent for ScoreEvent {
        fn to_persistable<C: Codec, M>(
            self,
            codec: &C,
            metadata: M,
        ) -> Result<PersistableEvent<M>, C::Error> {
            match self {
                Self::Added(e) => Ok(PersistableEvent {
                    kind: PointsAdded::KIND.to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
            }
        }
    }

    impl ProjectionEvent for ScoreEvent {
        const EVENT_KINDS: &'static [&'static str] = &[PointsAdded::KIND];

        fn from_stored<C: Codec>(
            kind: &str,
            data: &[u8],
            codec: &C,
        ) -> Result<Self, EventDecodeError<C::Error>> {
            match kind {
                "points-added" => Ok(Self::Added(
                    codec.deserialize(data).map_err(EventDecodeError::Codec)?,
                )),
                _ => Err(EventDecodeError::UnknownKind {
                    kind: kind.to_string(),
                    expected: Self::EVENT_KINDS,
                }),
            }
        }
    }

    impl From<PointsAdded> for ScoreEvent {
        fn from(e: PointsAdded) -> Self {
            Self::Added(e)
        }
    }

    #[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
    struct Score {
        total: i32,
    }

    impl Aggregate for Score {
        const KIND: &'static str = "score";
        type Event = ScoreEvent;
        type Error = String;
        type Id = String;

        fn apply(&mut self, event: &Self::Event) {
            match event {
                ScoreEvent::Added(e) => self.total += e.points,
            }
        }
    }

    #[tokio::test]
    async fn seed_events_appends_typed_events() {
        let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let mut repo = Repository::new(store);
        let id = "s1".to_string();

        repo.seed_events::<Score>(
            &id,
            vec![
                PointsAdded { points: 10 }.into(),
                PointsAdded { points: 20 }.into(),
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            repo.store().stream_version(Score::KIND, &id).await.unwrap(),
            Some(1)
        );

        // Verify events are loadable
        let loaded: Score = repo.aggregate_builder().load(&id).await.unwrap();
        assert_eq!(loaded.total, 30);
    }

    #[tokio::test]
    async fn inject_concurrent_event_appends_single_event() {
        let event_store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let mut repo = Repository::new(event_store);
        let id = "s1".to_string();

        // Seed initial state
        repo.seed_events::<Score>(&id, vec![PointsAdded { points: 100 }.into()])
            .await
            .unwrap();

        // Inject concurrent event
        repo.inject_concurrent_event::<Score>(&id, PointsAdded { points: 50 }.into())
            .await
            .unwrap();

        assert_eq!(
            repo.store().stream_version(Score::KIND, &id).await.unwrap(),
            Some(1)
        );

        // Verify both events are reflected
        let loaded: Score = repo.aggregate_builder().load(&id).await.unwrap();
        assert_eq!(loaded.total, 150);
    }

    #[tokio::test]
    async fn inject_raw_event_appends_raw_bytes() {
        let event_store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let mut repo = Repository::new(event_store);

        // Inject raw event with pre-serialized data
        repo.inject_raw_event(
            "score",
            &"s1".to_string(),
            PersistableEvent {
                kind: "points-added".to_string(),
                data: br#"{"points":42}"#.to_vec(),
                metadata: (),
            },
        )
        .await
        .unwrap();

        // Verify event is loadable
        let loaded: Score = repo
            .aggregate_builder()
            .load(&"s1".to_string())
            .await
            .unwrap();
        assert_eq!(loaded.total, 42);
    }
}
