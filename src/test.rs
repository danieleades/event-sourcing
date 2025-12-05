//! Test utilities for event-sourced aggregates.
//!
//! This module provides a test framework inspired by [cqrs-es](https://crates.io/crates/cqrs-es)
//! for testing aggregate behavior in isolation, without requiring a real event store.
//!
//! # Example
//!
//! ```ignore
//! use event_sourcing::test::TestFramework;
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

use std::fmt;
use std::marker::PhantomData;

use crate::{Aggregate, Handle};

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
    pub fn given(self, events: Vec<A::Event>) -> TestExecutor<A> {
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
            self.aggregate.apply(event);
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

    #[derive(Debug, Default)]
    struct Counter {
        value: i32,
    }

    impl Aggregate for Counter {
        const KIND: &'static str = "counter";
        type Event = CounterEvent;
        type Error = String;
        type Id = String;

        fn apply(&mut self, event: Self::Event) {
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
            .given(vec![CounterEvent::Added(ValueAdded { amount: 20 })])
            .when(&SubtractValue { amount: 5 })
            .then_expect_events(&[CounterEvent::Subtracted(ValueSubtracted { amount: 5 })]);
    }

    #[test]
    fn given_insufficient_balance_when_subtract_then_error() {
        CounterTest::new()
            .given(vec![CounterEvent::Added(ValueAdded { amount: 10 })])
            .when(&SubtractValue { amount: 20 })
            .then_expect_error();
    }

    #[test]
    fn given_insufficient_balance_when_subtract_then_error_message() {
        CounterTest::new()
            .given(vec![CounterEvent::Added(ValueAdded { amount: 10 })])
            .when(&SubtractValue { amount: 20 })
            .then_expect_error_message("insufficient value");
    }

    #[test]
    fn given_events_and_more_events_when_command() {
        CounterTest::new()
            .given(vec![CounterEvent::Added(ValueAdded { amount: 10 })])
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
}
