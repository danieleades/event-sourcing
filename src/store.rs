//! Persistence layer abstractions.
//!
//! This module describes the storage contract (`EventStore`), wire formats
//! (`PersistableEvent`, `StoredEvent`), transactions, and a reference
//! in-memory implementation. Filters and positions live here to keep storage
//! concerns together.
use std::collections::HashMap;
use std::convert::Infallible;

use crate::codec::{Codec, SerializableEvent};

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
/// - `Pos`: Position type for ordering (`()` for no ordering, `u64` for global sequence, etc.)
/// - `M`: Metadata type (defaults to `EventMetadata`)
#[derive(Clone)]
pub struct StoredEvent<Pos, M> {
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub kind: String,
    pub position: Pos,
    pub data: Vec<u8>,
    pub metadata: M,
}

/// Convenience alias for event batches loaded from a store.
pub type LoadEventsResult<Pos, Meta, Err> = Result<Vec<StoredEvent<Pos, Meta>>, Err>;

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
    pub(crate) const fn new(
        store: &'a mut S,
        aggregate_kind: String,
        aggregate_id: String,
    ) -> Self {
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

/// In-memory event store that keeps streams in a hash map.
///
/// Uses a global sequence counter (`Position = u64`) to maintain chronological
/// ordering across streams, enabling cross-aggregate projections that need to
/// interleave events by time rather than by stream name.
///
/// Generic over metadata type `M`, allowing zero-cost abstraction when metadata
/// is not needed (use `()`) or custom metadata types for specific use cases.
pub struct InMemoryEventStore<C, M>
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

impl crate::Codec for JsonCodec {
    type Error = serde_json::Error;

    fn serialize<E>(&self, event: &E) -> Result<Vec<u8>, Self::Error>
    where
        E: crate::event::DomainEvent,
    {
        serde_json::to_vec(event)
    }

    fn deserialize<E>(&self, data: &[u8]) -> Result<E, Self::Error>
    where
        E: crate::event::DomainEvent,
    {
        serde_json::from_slice(data)
    }
}
