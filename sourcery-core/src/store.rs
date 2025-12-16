//! Persistence layer abstractions.
//!
//! This module describes the storage contract (`EventStore`), wire formats
//! (`PersistableEvent`, `StoredEvent`), transactions, and a reference
//! in-memory implementation. Filters and positions live here to keep storage
//! concerns together.
use std::{future::Future, marker::PhantomData};

use thiserror::Error;

use crate::{
    codec::{Codec, SerializableEvent},
    concurrency::{ConcurrencyConflict, ConcurrencyStrategy, Optimistic, Unchecked},
};

pub mod inmemory;

/// Raw event data ready to be written to a store backend.
///
/// This is the boundary between Repository and `EventStore`. Repository
/// serializes events to this form, `EventStore` adds position and persistence.
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
/// - `Pos`: Position type for ordering (`()` for no ordering, `u64` for global
///   sequence, etc.)
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
/// Events are accumulated in the transaction and persisted atomically when
/// `commit()` is called. If the transaction is dropped without calling
/// `commit()`, the events are silently discarded (rolled back). This allows
/// errors during event serialization to be handled gracefully.
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
    pub const fn new(
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
    /// For sum-type events (enums), this serializes each variant to its
    /// persistable form.
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
        tracing::trace!(event_kind = %persistable.kind, "event appended to transaction");
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
    pub async fn commit(mut self) -> Result<(), S::Error> {
        let events = std::mem::take(&mut self.events);
        let event_count = events.len();
        tracing::debug!(
            aggregate_kind = %self.aggregate_kind,
            event_count,
            "committing transaction (unchecked)"
        );
        self.committed = true;
        self.store
            .append(&self.aggregate_kind, &self.aggregate_id, None, events)
            .await
            .map_err(|e| match e {
                AppendError::Store(e) => e,
                AppendError::Conflict(_) => unreachable!("conflict impossible without version"),
            })
    }
}

impl<S: EventStore> Transaction<'_, S, Optimistic> {
    /// Commit the transaction with version checking.
    ///
    /// The commit will fail with a [`ConcurrencyConflict`] if the stream
    /// version has changed since the aggregate was loaded.
    ///
    /// When `expected_version` is `None`, this means we expect a new aggregate
    /// (empty stream). The commit will fail with a conflict if the stream
    /// already has events.
    ///
    /// # Errors
    ///
    /// Returns [`AppendError::Conflict`] if another writer modified the stream,
    /// or [`AppendError::Store`] if persistence fails.
    pub async fn commit(mut self) -> Result<(), AppendError<S::Position, S::Error>> {
        let events = std::mem::take(&mut self.events);
        let event_count = events.len();
        tracing::debug!(
            aggregate_kind = %self.aggregate_kind,
            event_count,
            expected_version = ?self.expected_version,
            "committing transaction (optimistic)"
        );
        self.committed = true;

        match self.expected_version {
            Some(version) => {
                // Expected specific version - delegate to store's version checking
                self.store
                    .append(
                        &self.aggregate_kind,
                        &self.aggregate_id,
                        Some(version),
                        events,
                    )
                    .await
            }
            None => {
                // Expected new stream - verify stream is actually empty
                self.store
                    .append_expecting_new(&self.aggregate_kind, &self.aggregate_id, events)
                    .await
            }
        }
    }
}

impl<S: EventStore, C: ConcurrencyStrategy> Drop for Transaction<'_, S, C> {
    fn drop(&mut self) {
        if !self.committed && !self.events.is_empty() {
            tracing::trace!(
                aggregate_kind = %self.aggregate_kind,
                expected_version = ?self.expected_version,
                event_count = self.events.len(),
                "transaction dropped without commit; discarding buffered events"
            );
        }
    }
}

/// Abstraction over the persistence layer for event streams.
///
/// This trait supports both stream-based and type-partitioned storage
/// implementations.
///
/// Associated types allow stores to customize their behavior:
/// - `Id`: Aggregate identifier type
/// - `Position`: Ordering strategy (`()` for stream-based, `u64` for global
///   ordering)
/// - `Metadata`: Infrastructure metadata type (timestamps, causation tracking,
///   etc.)
/// - `Codec`: Serialization strategy for domain events
// ANCHOR: event_store_trait
pub trait EventStore: Send + Sync {
    /// Aggregate identifier type.
    ///
    /// This type must be clonable so repositories can reuse IDs across calls.
    /// Common choices: `String`, `Uuid`, or custom ID types.
    type Id: Clone + Send + Sync + 'static;

    /// Position type used for ordering events and version checking.
    ///
    /// Must be `Copy + PartialEq` to support optimistic concurrency.
    /// Use `()` if ordering is not needed.
    type Position: Copy + PartialEq + std::fmt::Debug + Send + Sync + 'static;

    /// Store-specific error type.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Serialization codec.
    type Codec: Codec + Clone + Send + Sync + 'static;

    /// Metadata type for infrastructure concerns.
    type Metadata: Send + Sync + 'static;

    fn codec(&self) -> &Self::Codec;

    /// Get the current version (latest position) for an aggregate stream.
    ///
    /// Returns `None` for streams with no events.
    ///
    /// # Errors
    ///
    /// Returns a store-specific error when the operation fails.
    fn stream_version<'a>(
        &'a self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
    ) -> impl Future<Output = Result<Option<Self::Position>, Self::Error>> + Send + 'a;

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
    fn append<'a>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        expected_version: Option<Self::Position>,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> impl Future<Output = Result<(), AppendError<Self::Position, Self::Error>>> + Send + 'a;

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
    fn load_events<'a>(
        &'a self,
        filters: &'a [EventFilter<Self::Id, Self::Position>],
    ) -> impl Future<
        Output = LoadEventsResult<Self::Id, Self::Position, Self::Metadata, Self::Error>,
    > + Send
    + 'a;

    /// Append events expecting an empty stream.
    ///
    /// This method is used by optimistic concurrency when creating new
    /// aggregates. It fails with a [`ConcurrencyConflict`] if the stream
    /// already has events.
    ///
    /// # Errors
    ///
    /// Returns [`AppendError::Conflict`] if the stream is not empty,
    /// or [`AppendError::Store`] if persistence fails.
    fn append_expecting_new<'a>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> impl Future<Output = Result<(), AppendError<Self::Position, Self::Error>>> + Send + 'a;
}
// ANCHOR_END: event_store_trait

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StreamKey<Id> {
    aggregate_kind: String,
    aggregate_id: Id,
}

impl<Id> StreamKey<Id> {
    pub(crate) fn new(aggregate_kind: impl Into<String>, aggregate_id: Id) -> Self {
        Self {
            aggregate_kind: aggregate_kind.into(),
            aggregate_id,
        }
    }
}

/// JSON codec backed by `serde_json`.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonCodec;

impl crate::codec::Codec for JsonCodec {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::inmemory;

    #[test]
    fn event_filter_for_event_is_unrestricted() {
        let filter: EventFilter<String> = EventFilter::for_event("my-event");
        assert_eq!(filter.event_kind, "my-event");
        assert_eq!(filter.aggregate_kind, None);
        assert_eq!(filter.aggregate_id, None);
        assert_eq!(filter.after_position, None);
    }

    #[test]
    fn event_filter_for_aggregate_is_restricted() {
        let filter: EventFilter<String> =
            EventFilter::for_aggregate("my-event", "my-aggregate", "123");
        assert_eq!(filter.event_kind, "my-event");
        assert_eq!(filter.aggregate_kind.as_deref(), Some("my-aggregate"));
        assert_eq!(filter.aggregate_id.as_deref(), Some("123"));
        assert_eq!(filter.after_position, None);
    }

    #[test]
    fn event_filter_after_sets_after_position() {
        let filter: EventFilter<String, u64> = EventFilter::for_event("e").after(10);
        assert_eq!(filter.after_position, Some(10));
    }

    #[test]
    fn json_codec_roundtrips() {
        #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        struct ValueAdded {
            amount: i32,
        }

        let codec = JsonCodec;
        let value = ValueAdded { amount: 42 };
        let bytes = codec.serialize(&value).unwrap();
        let decoded: ValueAdded = codec.deserialize(&bytes).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn json_codec_rejects_invalid_json() {
        #[derive(Debug, PartialEq, Eq, serde::Deserialize)]
        struct ValueAdded {
            amount: i32,
        }

        let codec = JsonCodec;
        let result: Result<ValueAdded, _> = codec.deserialize(b"not valid json");
        assert!(result.is_err());
    }

    #[test]
    fn json_codec_rejects_wrong_shape() {
        #[derive(Debug, PartialEq, Eq, serde::Deserialize)]
        struct ValueAdded {
            amount: i32,
        }

        let codec = JsonCodec;
        let result: Result<ValueAdded, _> = codec.deserialize(br#"{"wrong_field":123}"#);
        assert!(result.is_err());
    }

    async fn append_raw_event(
        store: &mut inmemory::Store<String, JsonCodec, ()>,
        aggregate_kind: &str,
        aggregate_id: &str,
        event_kind: &str,
        json_bytes: &[u8],
    ) {
        store
            .append(
                aggregate_kind,
                &aggregate_id.to_string(),
                None,
                vec![PersistableEvent {
                    kind: event_kind.to_string(),
                    data: json_bytes.to_vec(),
                    metadata: (),
                }],
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn in_memory_event_store_appends_and_loads_single_event() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let data = br#"{"amount":10}"#;

        append_raw_event(&mut store, "counter", "c1", "value-added", data).await;

        let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
        let events = store.load_events(&filters).await.unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].aggregate_kind, "counter");
        assert_eq!(events[0].aggregate_id, "c1");
        assert_eq!(events[0].kind, "value-added");
        assert_eq!(events[0].data, data);
        assert_eq!(events[0].position, 0);
    }

    #[tokio::test]
    async fn in_memory_event_store_loads_multiple_kinds_from_one_stream() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(
            &mut store,
            "counter",
            "c1",
            "value-added",
            br#"{"amount":10}"#,
        )
        .await;
        append_raw_event(
            &mut store,
            "counter",
            "c1",
            "value-subtracted",
            br#"{"amount":5}"#,
        )
        .await;

        let filters = vec![
            EventFilter::for_aggregate("value-added", "counter", "c1"),
            EventFilter::for_aggregate("value-subtracted", "counter", "c1"),
        ];
        let loaded = store.load_events(&filters).await.unwrap();

        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].kind, "value-added");
        assert_eq!(loaded[1].kind, "value-subtracted");
    }

    #[tokio::test]
    async fn in_memory_event_store_returns_empty_when_no_events_match() {
        let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let events = store
            .load_events(&[EventFilter::for_event("nonexistent")])
            .await
            .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn in_memory_event_store_filters_by_event_kind_and_aggregate_id() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await;
        append_raw_event(&mut store, "counter", "c2", "value-added", b"{}").await;

        let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
        let events = store.load_events(&filters).await.unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].aggregate_id, "c1");
    }

    #[tokio::test]
    async fn in_memory_event_store_orders_events_by_global_position() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await;
        append_raw_event(&mut store, "counter", "c2", "value-added", b"{}").await;
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await;

        let events = store
            .load_events(&[EventFilter::for_event("value-added")])
            .await
            .unwrap();

        let positions: Vec<u64> = events.iter().map(|e| e.position).collect();
        assert_eq!(positions, vec![0, 1, 2]);
        assert_eq!(events[0].aggregate_id, "c1");
        assert_eq!(events[1].aggregate_id, "c2");
        assert_eq!(events[2].aggregate_id, "c1");
    }

    #[tokio::test]
    async fn in_memory_event_store_deduplicates_overlapping_filters() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await;

        let filters = vec![
            EventFilter::for_aggregate("value-added", "counter", "c1"),
            EventFilter::for_event("value-added"),
        ];
        let events = store.load_events(&filters).await.unwrap();
        assert_eq!(events.len(), 1);
    }

    #[tokio::test]
    async fn in_memory_event_store_applies_after_position_filter() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await; // pos 0
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await; // pos 1
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await; // pos 2

        let events = store
            .load_events(&[EventFilter::for_event("value-added").after(1)])
            .await
            .unwrap();

        let positions: Vec<u64> = events.iter().map(|e| e.position).collect();
        assert_eq!(positions, vec![2]);
    }

    #[tokio::test]
    async fn in_memory_event_store_stream_version_is_none_for_empty_stream() {
        let store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let version = store
            .stream_version("counter", &"nonexistent".to_string())
            .await
            .unwrap();
        assert_eq!(version, None);
    }

    #[tokio::test]
    async fn in_memory_event_store_version_checking_detects_conflict() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "value-added", b"{}").await;

        let ok = store
            .append(
                "counter",
                &"c1".to_string(),
                Some(0),
                vec![PersistableEvent {
                    kind: "value-added".to_string(),
                    data: b"{}".to_vec(),
                    metadata: (),
                }],
            )
            .await;
        assert!(ok.is_ok());

        let conflict = store
            .append(
                "counter",
                &"c1".to_string(),
                Some(0),
                vec![PersistableEvent {
                    kind: "value-added".to_string(),
                    data: b"{}".to_vec(),
                    metadata: (),
                }],
            )
            .await;
        assert!(matches!(conflict, Err(AppendError::Conflict(_))));
    }

    #[derive(Clone, Debug, serde::Serialize)]
    struct TestAdded {
        amount: i32,
    }

    #[derive(Clone, Debug)]
    enum TestEvent {
        Added(TestAdded),
    }

    impl crate::codec::SerializableEvent for TestEvent {
        fn to_persistable<C: crate::codec::Codec, M>(
            self,
            codec: &C,
            metadata: M,
        ) -> Result<PersistableEvent<M>, C::Error> {
            match self {
                Self::Added(e) => Ok(PersistableEvent {
                    kind: "added".to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
            }
        }
    }

    #[tokio::test]
    async fn unchecked_transaction_commit_persists_events() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        let mut tx =
            store.begin::<crate::concurrency::Unchecked>("counter", "c1".to_string(), None);
        tx.append(TestEvent::Added(TestAdded { amount: 10 }), ())
            .unwrap();
        tx.commit().await.unwrap();

        let events = store
            .load_events(&[EventFilter::for_aggregate("added", "counter", "c1")])
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].position, 0);
        assert_eq!(events[0].kind, "added");
    }

    #[tokio::test]
    async fn dropping_transaction_without_commit_discards_buffered_events() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        {
            let mut tx =
                store.begin::<crate::concurrency::Unchecked>("counter", "c1".to_string(), None);
            tx.append(TestEvent::Added(TestAdded { amount: 10 }), ())
                .unwrap();
        }

        let events = store
            .load_events(&[EventFilter::for_event("added")])
            .await
            .unwrap();
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn optimistic_transaction_detects_stale_expected_version() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "added", br#"{"amount":10}"#).await;

        let mut tx =
            store.begin::<crate::concurrency::Optimistic>("counter", "c1".to_string(), Some(999));
        tx.append(TestEvent::Added(TestAdded { amount: 1 }), ())
            .unwrap();

        let result = tx.commit().await;
        assert!(matches!(result, Err(AppendError::Conflict(_))));
    }

    #[tokio::test]
    async fn optimistic_transaction_detects_non_new_stream_when_expect_new() {
        let mut store: inmemory::Store<String, JsonCodec, ()> = inmemory::Store::new(JsonCodec);
        append_raw_event(&mut store, "counter", "c1", "added", br#"{"amount":10}"#).await;

        let mut tx =
            store.begin::<crate::concurrency::Optimistic>("counter", "c1".to_string(), None);
        tx.append(TestEvent::Added(TestAdded { amount: 1 }), ())
            .unwrap();

        let result = tx.commit().await;
        assert!(matches!(result, Err(AppendError::Conflict(_))));
    }
}
