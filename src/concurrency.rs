//! Compile-time concurrency strategy selection.
//!
//! This module provides marker types for choosing between unchecked (last-writer-wins)
//! and optimistic (version-checked) concurrency control at the type level.
//!
//! # Example
//!
//! ```ignore
//! // Default: no version checking
//! let repo = Repository::new(store);
//!
//! // Opt-in to optimistic concurrency
//! let repo = Repository::new(store).with_optimistic_concurrency();
//! ```

use std::fmt;

use thiserror::Error;

/// No version checking - last writer wins.
///
/// This is the default concurrency strategy. Events are appended without
/// checking whether other events were added since loading. Suitable for
/// single-writer scenarios or when conflicts are acceptable.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unchecked;

/// Optimistic concurrency control - version checked on every write.
///
/// With this strategy, the repository tracks the stream version when loading
/// an aggregate and verifies it hasn't changed before appending new events.
/// If the version changed (another writer appended events), the operation
/// fails with a [`ConcurrencyConflict`] error.
#[derive(Debug, Clone, Copy, Default)]
pub struct Optimistic;

/// Sealed trait for concurrency strategy markers.
///
/// This trait cannot be implemented outside this crate, ensuring only
/// [`Unchecked`] and [`Optimistic`] can be used as concurrency strategies.
pub trait ConcurrencyStrategy: private::Sealed + Default {
    /// Whether this strategy checks versions before appending.
    const CHECK_VERSION: bool;
}

impl ConcurrencyStrategy for Unchecked {
    const CHECK_VERSION: bool = false;
}

impl ConcurrencyStrategy for Optimistic {
    const CHECK_VERSION: bool = true;
}

mod private {
    pub trait Sealed {}
    impl Sealed for super::Unchecked {}
    impl Sealed for super::Optimistic {}
}

/// Error indicating a concurrency conflict during append.
///
/// This error is returned when using [`Optimistic`] concurrency and another
/// writer has appended events to the stream since we loaded the aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}", format_conflict(.expected.as_ref(), .actual.as_ref()))]
pub struct ConcurrencyConflict<Pos: fmt::Debug> {
    /// The version we expected (from when we loaded the aggregate).
    /// `None` indicates we expected a new/empty stream.
    pub expected: Option<Pos>,
    /// The actual current version in the store.
    /// `None` indicates the stream is empty (which shouldn't happen in a conflict).
    pub actual: Option<Pos>,
}

fn format_conflict<Pos: fmt::Debug>(expected: Option<&Pos>, actual: Option<&Pos>) -> String {
    match (expected, actual) {
        (None, Some(actual)) => {
            format!("concurrency conflict: expected new stream, found version {actual:?}")
        }
        (Some(expected), actual) => {
            format!("concurrency conflict: expected version {expected:?}, found {actual:?}")
        }
        (None, None) => "concurrency conflict: unexpected state".to_string(),
    }
}
