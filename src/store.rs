//! Persistence layer abstractions.
//!
//! This module describes the storage contract (`EventStore`), wire formats
//! (`PersistableEvent`, `StoredEvent`), transactions, and a reference
//! in-memory implementation. Filters and positions live here to keep storage
//! concerns together.
use std::collections::HashMap;
use std::convert::Infallible;
use std::marker::PhantomData;

use thiserror::Error;

use crate::codec::{Codec, SerializableEvent};
use crate::concurrency::{ConcurrencyConflict, ConcurrencyStrategy, Optimistic, Unchecked};

/// Raw event data ready to be written to a store backend.
///
/// This is the boundary between Repository and `EventStore`. Repository serializes
/// events to this form, `EventStore` adds position and persistence.
///
/// Generic over metadata type `M` to support different metadata structures.
#[derive(Clone)]
pub struct PersistableEvent<M> {
    pub kind: String,
    pub data: Vec<u8>,
    pub metadata: M,
}

/// Event materialized from the store, with position.
///
/// Generic parameters:
/// - `Id`: Aggregate identifier type
/// - `Pos`: Position type for ordering (`()` for no ordering, `u64` for global sequence, etc.)
/// - `M`: Metadata type (defaults to `EventMetadata`)
#[derive(Clone)]
pub struct StoredEvent<Id, Pos, M> {
    pub aggregate_kind: String,
    pub aggregate_id: Id,
    pub kind: String,
    pub position: Pos,
    pub data: Vec<u8>,
    pub metadata: M,
}

/// Convenience alias for event batches loaded from a store.
pub type LoadEventsResult<Id, Pos, Meta, Err> = Result<Vec<StoredEvent<Id, Pos, Meta>>, Err>;

/// Filter describing which events should be loaded from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventFilter<Id, Pos = ()> {
    pub event_kind: String,
    pub aggregate_kind: Option<String>,
    pub aggregate_id: Option<Id>,
    /// Only load events with position strictly greater than this value.
    /// Used for snapshot-based loading to skip already-applied events.
    pub after_position: Option<Pos>,
}

impl<Id, Pos> EventFilter<Id, Pos> {
    /// Load all events of the specified kind across every aggregate.
    #[must_use]
    pub fn for_event(kind: impl Into<String>) -> Self {
        Self {
            event_kind: kind.into(),
            aggregate_kind: None,
            aggregate_id: None,
            after_position: None,
        }
    }

    /// Load events of the specified kind for a single aggregate instance.
    #[must_use]
    pub fn for_aggregate(
        event_kind: impl Into<String>,
        aggregate_kind: impl Into<String>,
        aggregate_id: impl Into<Id>,
    ) -> Self {
        Self {
            event_kind: event_kind.into(),
            aggregate_kind: Some(aggregate_kind.into()),
            aggregate_id: Some(aggregate_id.into()),
            after_position: None,
        }
    }

    /// Only load events with position strictly greater than the given value.
    ///
    /// This is used for snapshot-based loading: load a snapshot at position N,
    /// then load events with `after(N)` to get only the events that occurred
    /// after the snapshot was taken.
    #[must_use]
    pub fn after(mut self, position: Pos) -> Self {
        self.after_position = Some(position);
        self
    }
}

/// Error from append operations with version checking.
#[derive(Debug, Error)]
pub enum AppendError<Pos, StoreError>
where
    Pos: std::fmt::Debug,
    StoreError: std::error::Error,
{
    /// Concurrency conflict - another writer modified the stream.
    #[error(transparent)]
    Conflict(#[from] ConcurrencyConflict<Pos>),
    /// Underlying store error.
    #[error("store error: {0}")]
    Store(#[source] StoreError),
}

impl<Pos: std::fmt::Debug, StoreError: std::error::Error> AppendError<Pos, StoreError> {
    /// Create a store error variant.
    pub const fn store(err: StoreError) -> Self {
        Self::Store(err)
    }
}

/// Transaction for appending events to an aggregate instance.
///
/// Events are accumulated in the transaction and persisted atomically when `commit()` is called.
/// If the transaction is dropped without calling `commit()`, the events are silently discarded
/// (rolled back). This allows errors during event serialization to be handled gracefully.
///
/// The `C` type parameter determines the concurrency strategy:
/// - [`Unchecked`]: No version checking (default)
/// - [`Optimistic`]: Version checked on commit
pub struct Transaction<'a, S: EventStore, C: ConcurrencyStrategy = Unchecked> {
    store: &'a mut S,
    aggregate_kind: String,
    aggregate_id: S::Id,
    expected_version: Option<S::Position>,
    events: Vec<PersistableEvent<S::Metadata>>,
    committed: bool,
    _concurrency: PhantomData<C>,
}

impl<'a, S: EventStore, C: ConcurrencyStrategy> Transaction<'a, S, C> {
    pub(crate) const fn new(
        store: &'a mut S,
        aggregate_kind: String,
        aggregate_id: S::Id,
        expected_version: Option<S::Position>,
    ) -> Self {
        Self {
            store,
            aggregate_kind,
            aggregate_id,
            expected_version,
            events: Vec::new(),
            committed: false,
            _concurrency: PhantomData,
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
}

impl<S: EventStore> Transaction<'_, S, Unchecked> {
    /// Commit the transaction without version checking.
    ///
    /// Events are persisted atomically. No conflict detection is performed.
    ///
    /// # Errors
    ///
    /// Returns a store error if persistence fails.
    pub fn commit(mut self) -> Result<(), S::Error> {
        let events = std::mem::take(&mut self.events);
        self.committed = true;
        self.store
            .append(&self.aggregate_kind, &self.aggregate_id, None, events)
            .map_err(|e| match e {
                AppendError::Store(e) => e,
                AppendError::Conflict(_) => unreachable!("conflict impossible without version"),
            })
    }
}

impl<S: EventStore> Transaction<'_, S, Optimistic> {
    /// Commit the transaction with version checking.
    ///
    /// The commit will fail with a [`ConcurrencyConflict`] if the stream version
    /// has changed since the aggregate was loaded.
    ///
    /// When `expected_version` is `None`, this means we expect a new aggregate
    /// (empty stream). The commit will fail with a conflict if the stream already
    /// has events.
    ///
    /// # Errors
    ///
    /// Returns [`AppendError::Conflict`] if another writer modified the stream,
    /// or [`AppendError::Store`] if persistence fails.
    pub fn commit(mut self) -> Result<(), AppendError<S::Position, S::Error>> {
        let events = std::mem::take(&mut self.events);
        self.committed = true;

        match self.expected_version {
            Some(version) => {
                // Expected specific version - delegate to store's version checking
                self.store.append(
                    &self.aggregate_kind,
                    &self.aggregate_id,
                    Some(version),
                    events,
                )
            }
            None => {
                // Expected new stream - verify stream is actually empty
                self.store
                    .append_expecting_new(&self.aggregate_kind, &self.aggregate_id, events)
            }
        }
    }
}

impl<S: EventStore, C: ConcurrencyStrategy> Drop for Transaction<'_, S, C> {
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
/// - `Id`: Aggregate identifier type
/// - `Position`: Ordering strategy (`()` for stream-based, `u64` for global ordering)
/// - `Metadata`: Infrastructure metadata type (timestamps, causation tracking, etc.)
/// - `Codec`: Serialization strategy for domain events
pub trait EventStore {
    /// Aggregate identifier type.
    ///
    /// This type must be clonable for storage and hashable/equatable for lookups.
    /// Common choices: `String`, `Uuid`, or custom ID types.
    type Id: Clone + Eq + std::hash::Hash;

    /// Position type used for ordering events and version checking.
    ///
    /// Must be `Copy + PartialEq` to support optimistic concurrency.
    /// Use `()` if ordering is not needed.
    type Position: Copy + PartialEq + std::fmt::Debug;

    /// Store-specific error type.
    type Error: std::error::Error;

    /// Serialization codec.
    type Codec: Codec;

    /// Metadata type for infrastructure concerns.
    type Metadata;

    fn codec(&self) -> &Self::Codec;

    /// Get the current version (latest position) for an aggregate stream.
    ///
    /// Returns `None` for streams with no events.
    ///
    /// # Errors
    ///
    /// Returns a store-specific error when the operation fails.
    fn stream_version(
        &self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
    ) -> Result<Option<Self::Position>, Self::Error>;

    /// Begin a transaction for appending events to an aggregate.
    ///
    /// The transaction type is determined by the concurrency strategy `C`.
    ///
    /// # Arguments
    /// * `aggregate_kind` - The aggregate type identifier (`Aggregate::KIND`)
    /// * `aggregate_id` - The aggregate instance identifier
    /// * `expected_version` - The version expected for optimistic concurrency
    fn begin<C: ConcurrencyStrategy>(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: Self::Id,
        expected_version: Option<Self::Position>,
    ) -> Transaction<'_, Self, C>
    where
        Self: Sized;

    /// Append events with optional version checking.
    ///
    /// If `expected_version` is `Some`, the append fails with a concurrency
    /// conflict if the current stream version doesn't match.
    /// If `expected_version` is `None`, no version checking is performed.
    ///
    /// # Errors
    ///
    /// Returns [`AppendError::Conflict`] if the version doesn't match, or
    /// [`AppendError::Store`] if persistence fails.
    fn append(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
        expected_version: Option<Self::Position>,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), AppendError<Self::Position, Self::Error>>;

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
        filters: &[EventFilter<Self::Id, Self::Position>],
    ) -> LoadEventsResult<Self::Id, Self::Position, Self::Metadata, Self::Error>;

    /// Append events expecting an empty stream.
    ///
    /// This method is used by optimistic concurrency when creating new aggregates.
    /// It fails with a [`ConcurrencyConflict`] if the stream already has events.
    ///
    /// # Errors
    ///
    /// Returns [`AppendError::Conflict`] if the stream is not empty,
    /// or [`AppendError::Store`] if persistence fails.
    fn append_expecting_new(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), AppendError<Self::Position, Self::Error>>;
}

/// In-memory event store that keeps streams in a hash map.
///
/// Uses a global sequence counter (`Position = u64`) to maintain chronological
/// ordering across streams, enabling cross-aggregate projections that need to
/// interleave events by time rather than by stream name.
///
/// Generic over:
/// - `Id`: Aggregate identifier type (must be displayable for stream key generation)
/// - `C`: Serialization codec
/// - `M`: Metadata type (use `()` when not needed)
pub struct InMemoryEventStore<Id, C, M>
where
    C: Codec,
{
    codec: C,
    streams: HashMap<String, Vec<StoredEvent<Id, u64, M>>>,
    next_position: u64,
}

impl<Id, C, M> InMemoryEventStore<Id, C, M>
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

/// Infallible error type that implements `std::error::Error`.
///
/// Used by [`InMemoryEventStore`] which cannot fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("infallible")]
pub enum InMemoryError {}

impl From<Infallible> for InMemoryError {
    fn from(x: Infallible) -> Self {
        match x {}
    }
}

impl<Id, C, M> EventStore for InMemoryEventStore<Id, C, M>
where
    Id: Clone + Eq + std::hash::Hash + std::fmt::Display,
    C: Codec,
    M: Clone,
{
    type Id = Id;
    type Position = u64; // Global sequence for chronological ordering
    type Error = InMemoryError;
    type Codec = C;
    type Metadata = M;

    fn codec(&self) -> &Self::Codec {
        &self.codec
    }

    fn stream_version(
        &self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
    ) -> Result<Option<u64>, Self::Error> {
        let stream_key = format!("{aggregate_kind}::{aggregate_id}");
        Ok(self
            .streams
            .get(&stream_key)
            .and_then(|s| s.last().map(|e| e.position)))
    }

    fn begin<Conc: ConcurrencyStrategy>(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: Self::Id,
        expected_version: Option<Self::Position>,
    ) -> Transaction<'_, Self, Conc> {
        Transaction::new(
            self,
            aggregate_kind.to_string(),
            aggregate_id,
            expected_version,
        )
    }

    fn append(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
        expected_version: Option<u64>,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), AppendError<u64, Self::Error>> {
        // Check version if provided
        if let Some(expected) = expected_version {
            let current = self
                .stream_version(aggregate_kind, aggregate_id)
                .map_err(AppendError::store)?;
            if current != Some(expected) {
                return Err(ConcurrencyConflict {
                    expected: Some(expected),
                    actual: current,
                }
                .into());
            }
        }

        let stream_key = format!("{aggregate_kind}::{aggregate_id}");
        let stored: Vec<StoredEvent<Id, u64, M>> = events
            .into_iter()
            .map(|e| {
                let position = self.next_position;
                self.next_position += 1;
                StoredEvent {
                    aggregate_kind: aggregate_kind.to_string(),
                    aggregate_id: aggregate_id.clone(),
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

    fn append_expecting_new(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: &Self::Id,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), AppendError<u64, Self::Error>> {
        // Check that stream is empty (new aggregate)
        let current = self
            .stream_version(aggregate_kind, aggregate_id)
            .map_err(AppendError::store)?;

        if let Some(actual) = current {
            // Stream already has events - conflict!
            return Err(ConcurrencyConflict {
                expected: None, // "expected new stream"
                actual: Some(actual),
            }
            .into());
        }

        // Stream is empty, proceed with append (no further version check needed)
        let stream_key = format!("{aggregate_kind}::{aggregate_id}");
        let stored: Vec<StoredEvent<Id, u64, M>> = events
            .into_iter()
            .map(|e| {
                let position = self.next_position;
                self.next_position += 1;
                StoredEvent {
                    aggregate_kind: aggregate_kind.to_string(),
                    aggregate_id: aggregate_id.clone(),
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
        filters: &[EventFilter<Self::Id, Self::Position>],
    ) -> Result<Vec<StoredEvent<Id, u64, M>>, Self::Error> {
        use std::collections::HashSet;

        let mut result = Vec::new();
        let mut seen: HashSet<(String, Id, String)> = HashSet::new(); // (aggregate_kind, aggregate_id, event_kind)

        // Group filters by aggregate ID, tracking each filter's individual position constraint
        // Maps event_kind -> after_position for that specific filter
        let mut all_kinds: HashMap<String, Option<u64>> = HashMap::new(); // Filters with no aggregate restriction
        let mut by_aggregate: HashMap<(String, Id), HashMap<String, Option<u64>>> = HashMap::new(); // Filters targeting a specific aggregate

        for filter in filters {
            if let (Some(kind), Some(id)) = (&filter.aggregate_kind, &filter.aggregate_id) {
                by_aggregate
                    .entry((kind.clone(), id.clone()))
                    .or_default()
                    .insert(filter.event_kind.clone(), filter.after_position);
            } else {
                all_kinds.insert(filter.event_kind.clone(), filter.after_position);
            }
        }

        // Helper to check position filter for a specific after_position constraint
        let passes_position_filter =
            |event: &StoredEvent<Id, u64, M>, after_position: Option<u64>| -> bool {
                after_position.is_none_or(|after| event.position > after)
            };

        // Load events for specific aggregates
        for ((aggregate_kind, aggregate_id), kinds) in &by_aggregate {
            let stream_key = format!("{aggregate_kind}::{aggregate_id}");
            if let Some(stream) = self.streams.get(&stream_key) {
                for event in stream {
                    // Check if this event kind is requested AND passes its specific position filter
                    if let Some(&after_pos) = kinds.get(&event.kind)
                        && passes_position_filter(event, after_pos)
                    {
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
        }

        // Load events from all aggregates for unfiltered kinds
        // Skip events we've already loaded for specific aggregates
        if !all_kinds.is_empty() {
            for stream in self.streams.values() {
                for event in stream {
                    // Check if this event kind is requested AND passes its specific position filter
                    if let Some(&after_pos) = all_kinds.get(&event.kind)
                        && passes_position_filter(event, after_pos)
                    {
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
        }

        // Sort by position for chronological ordering across streams
        result.sort_by_key(|event| event.position);

        Ok(result)
    }
}

/// JSON codec backed by `serde_json`.
pub struct JsonCodec;

impl crate::Codec for JsonCodec {
    type Error = serde_json::Error;

    fn serialize<T>(&self, value: &T) -> Result<Vec<u8>, Self::Error>
    where
        T: serde::Serialize,
    {
        serde_json::to_vec(value)
    }

    fn deserialize<T>(&self, data: &[u8]) -> Result<T, Self::Error>
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_slice(data)
    }
}
