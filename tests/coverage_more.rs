#![cfg(feature = "test-util")]

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};

use event_sourcing::codec::{Codec, EventDecodeError, ProjectionEvent, SerializableEvent};
use event_sourcing::concurrency::ConcurrencyConflict;
use event_sourcing::projection::ProjectionError;
use event_sourcing::repository::{OptimisticCommandError, SnapshotCommandError};
use event_sourcing::snapshot::{
    InMemorySnapshotStore, OfferSnapshotError, Snapshot, SnapshotOffer, SnapshotStore,
};
use event_sourcing::store::{EventStore, JsonCodec, PersistableEvent, inmemory};
use event_sourcing::test::RepositoryTestExt;
use event_sourcing::{Aggregate, ApplyProjection, DomainEvent, Handle, Projection, Repository};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ValueAdded {
    amount: i32,
}

impl DomainEvent for ValueAdded {
    const KIND: &'static str = "value-added";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum CounterEvent {
    Added(ValueAdded),
}

impl SerializableEvent for CounterEvent {
    fn to_persistable<C: Codec, M>(
        self,
        codec: &C,
        metadata: M,
    ) -> Result<PersistableEvent<M>, C::Error> {
        match self {
            Self::Added(event) => Ok(PersistableEvent {
                kind: ValueAdded::KIND.to_string(),
                data: codec.serialize(&event)?,
                metadata,
            }),
        }
    }
}

impl ProjectionEvent for CounterEvent {
    const EVENT_KINDS: &'static [&'static str] = &[ValueAdded::KIND];

    fn from_stored<C: Codec>(
        kind: &str,
        data: &[u8],
        codec: &C,
    ) -> Result<Self, EventDecodeError<C::Error>> {
        match kind {
            ValueAdded::KIND => Ok(Self::Added(
                codec.deserialize(data).map_err(EventDecodeError::Codec)?,
            )),
            _ => Err(EventDecodeError::UnknownKind {
                kind: kind.to_string(),
                expected: Self::EVENT_KINDS,
            }),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        }
    }
}

struct AddValue {
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

struct NoOp;

impl Handle<NoOp> for Counter {
    fn handle(&self, _: &NoOp) -> Result<Vec<Self::Event>, Self::Error> {
        Ok(vec![])
    }
}

struct RequireAtLeast {
    min: i32,
}

impl Handle<RequireAtLeast> for Counter {
    fn handle(&self, command: &RequireAtLeast) -> Result<Vec<Self::Event>, Self::Error> {
        if self.value < command.min {
            return Err("insufficient value".to_string());
        }
        Ok(vec![])
    }
}

#[derive(Debug, Default)]
struct TotalsProjection {
    totals: HashMap<String, i32>,
}

impl Projection for TotalsProjection {
    type Id = String;
    type Metadata = ();
}

impl ApplyProjection<ValueAdded> for TotalsProjection {
    fn apply_projection(
        &mut self,
        aggregate_id: &Self::Id,
        event: &ValueAdded,
        &(): &Self::Metadata,
    ) {
        *self.totals.entry(aggregate_id.clone()).or_insert(0) += event.amount;
    }
}

#[test]
fn concurrency_conflict_formats_expected_new_stream_hint() {
    let conflict = ConcurrencyConflict::<u64> {
        expected: None,
        actual: Some(42),
    };
    let msg = conflict.to_string();
    assert!(msg.contains("expected new stream"));
}

#[test]
fn concurrency_conflict_formats_unexpected_empty_state() {
    let conflict = ConcurrencyConflict::<u64> {
        expected: None,
        actual: None,
    };
    let msg = conflict.to_string();
    assert!(msg.contains("unexpected empty state"));
}

#[tokio::test]
async fn in_memory_snapshot_store_policy_always_saves() {
    let mut snapshots = InMemorySnapshotStore::<String, u64>::always();
    let id = "c1".to_string();

    let result = snapshots
        .offer_snapshot(
            Counter::KIND,
            &id,
            0,
            || -> Result<Snapshot<u64>, Infallible> {
                Ok(Snapshot {
                    position: 1,
                    data: vec![1, 2, 3],
                })
            },
        )
        .await
        .unwrap();
    assert_eq!(result, SnapshotOffer::Stored);

    let loaded = snapshots.load(Counter::KIND, &id).await.unwrap();
    assert!(loaded.is_some());
}

#[tokio::test]
async fn in_memory_snapshot_store_policy_every_n_events_saves_at_threshold() {
    let mut snapshots = InMemorySnapshotStore::<String, u64>::every(3);
    let id = "c1".to_string();

    let result = snapshots
        .offer_snapshot(
            Counter::KIND,
            &id,
            2,
            || -> Result<Snapshot<u64>, Infallible> {
                panic!("snapshot should be declined before threshold");
            },
        )
        .await
        .unwrap();
    assert_eq!(result, SnapshotOffer::Declined);

    assert!(snapshots.load(Counter::KIND, &id).await.unwrap().is_none());

    let result = snapshots
        .offer_snapshot(
            Counter::KIND,
            &id,
            3,
            || -> Result<Snapshot<u64>, Infallible> {
                Ok(Snapshot {
                    position: 2,
                    data: vec![2],
                })
            },
        )
        .await
        .unwrap();
    assert_eq!(result, SnapshotOffer::Stored);
    assert!(snapshots.load(Counter::KIND, &id).await.unwrap().is_some());
}

#[tokio::test]
async fn in_memory_snapshot_store_policy_never_does_not_save() {
    let mut snapshots = InMemorySnapshotStore::<String, u64>::never();
    let id = "c1".to_string();

    let result = snapshots
        .offer_snapshot(
            Counter::KIND,
            &id,
            100,
            || -> Result<Snapshot<u64>, Infallible> {
                panic!("snapshot should be declined when policy is never");
            },
        )
        .await
        .unwrap();
    assert_eq!(result, SnapshotOffer::Declined);

    assert!(snapshots.load(Counter::KIND, &id).await.unwrap().is_none());
}

#[tokio::test]
async fn projection_event_for_filters_by_aggregate() {
    let store = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store);

    repo.execute_command::<Counter, AddValue>(&"c1".to_string(), &AddValue { amount: 10 }, &())
        .await
        .unwrap();
    repo.execute_command::<Counter, AddValue>(&"c2".to_string(), &AddValue { amount: 20 }, &())
        .await
        .unwrap();

    let projection: TotalsProjection = repo
        .build_projection()
        .event_for::<Counter, ValueAdded>(&"c1".to_string())
        .load()
        .await
        .unwrap();

    assert_eq!(projection.totals.get("c1"), Some(&10));
    assert!(!projection.totals.contains_key("c2"));
}

#[tokio::test]
async fn projection_load_surfaces_codec_error_with_event_kind() {
    let store = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store);

    repo.inject_raw_event(
        Counter::KIND,
        &"c1".to_string(),
        PersistableEvent {
            kind: ValueAdded::KIND.to_string(),
            data: b"not-json".to_vec(),
            metadata: (),
        },
    )
    .await
    .unwrap();

    let err = repo
        .build_projection::<TotalsProjection>()
        .event::<ValueAdded>()
        .load()
        .await
        .unwrap_err();

    match err {
        ProjectionError::Codec { event_kind, .. } => assert_eq!(event_kind, ValueAdded::KIND),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn unchecked_repository_saves_snapshot_and_exposes_snapshot_store() {
    let store = inmemory::Store::new(JsonCodec);
    let snapshots = InMemorySnapshotStore::always();
    let mut repo = Repository::new(store)
        .with_snapshots(snapshots)
        .without_concurrency_checking();

    let id = "c1".to_string();
    repo.execute_command::<Counter, AddValue>(&id, &AddValue { amount: 5 }, &())
        .await
        .unwrap();

    let loaded = repo
        .snapshot_store()
        .load(Counter::KIND, &id)
        .await
        .unwrap();
    assert!(loaded.is_some());
}

#[tokio::test]
async fn unchecked_execute_command_with_no_events_does_not_persist_or_snapshot() {
    let store = inmemory::Store::new(JsonCodec);
    let snapshots = InMemorySnapshotStore::always();
    let mut repo = Repository::new(store)
        .with_snapshots(snapshots)
        .without_concurrency_checking();

    let id = "c1".to_string();
    repo.execute_command::<Counter, NoOp>(&id, &NoOp, &())
        .await
        .unwrap();

    assert!(
        repo.event_store()
            .stream_version(Counter::KIND, &id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.snapshot_store()
            .load(Counter::KIND, &id)
            .await
            .unwrap()
            .is_none()
    );
}

#[derive(Debug, Error)]
#[error("snapshot load failed")]
struct SnapshotLoadError;

#[derive(Debug)]
struct FailingLoadSnapshotStore;

impl SnapshotStore for FailingLoadSnapshotStore {
    type Id = String;
    type Position = u64;
    type Error = SnapshotLoadError;

    fn load<'a>(
        &'a self,
        _: &'a str,
        _: &'a Self::Id,
    ) -> impl std::future::Future<Output = Result<Option<Snapshot<Self::Position>>, Self::Error>>
    + Send
    + 'a {
        std::future::ready(Err(SnapshotLoadError))
    }

    fn offer_snapshot<'a, CE, Create>(
        &'a mut self,
        _: &'a str,
        _: &'a Self::Id,
        _: u64,
        _: Create,
    ) -> impl std::future::Future<
        Output = Result<SnapshotOffer, OfferSnapshotError<Self::Error, CE>>,
    > + Send
    + 'a
    where
        CE: std::error::Error + Send + Sync + 'static,
        Create: FnOnce() -> Result<Snapshot<Self::Position>, CE> + 'a,
    {
        std::future::ready(Ok(SnapshotOffer::Declined))
    }
}

#[tokio::test]
async fn snapshot_load_failure_falls_back_to_full_replay() {
    let store = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store)
        .with_snapshots(FailingLoadSnapshotStore)
        .without_concurrency_checking();

    let id = "c1".to_string();
    repo.execute_command::<Counter, AddValue>(&id, &AddValue { amount: 10 }, &())
        .await
        .unwrap();

    repo.execute_command::<Counter, RequireAtLeast>(&id, &RequireAtLeast { min: 5 }, &())
        .await
        .unwrap();
}

#[derive(Debug, Default)]
struct CorruptSnapshotStore;

impl SnapshotStore for CorruptSnapshotStore {
    type Id = String;
    type Position = u64;
    type Error = SnapshotLoadError;

    fn load<'a>(
        &'a self,
        _: &'a str,
        _: &'a Self::Id,
    ) -> impl std::future::Future<Output = Result<Option<Snapshot<Self::Position>>, Self::Error>>
    + Send
    + 'a {
        std::future::ready(Ok(Some(Snapshot {
            position: 0,
            data: b"not-json".to_vec(),
        })))
    }

    fn offer_snapshot<'a, CE, Create>(
        &'a mut self,
        _: &'a str,
        _: &'a Self::Id,
        _: u64,
        _: Create,
    ) -> impl std::future::Future<
        Output = Result<SnapshotOffer, OfferSnapshotError<Self::Error, CE>>,
    > + Send
    + 'a
    where
        CE: std::error::Error + Send + Sync + 'static,
        Create: FnOnce() -> Result<Snapshot<Self::Position>, CE> + 'a,
    {
        std::future::ready(Ok(SnapshotOffer::Declined))
    }
}

#[tokio::test]
async fn corrupt_snapshot_data_returns_projection_error() {
    let store = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store)
        .with_snapshots(CorruptSnapshotStore)
        .without_concurrency_checking();

    let err = repo
        .execute_command::<Counter, AddValue>(&"c1".to_string(), &AddValue { amount: 1 }, &())
        .await
        .unwrap_err();

    assert!(matches!(err, SnapshotCommandError::Projection(_)));
}

#[tokio::test]
async fn execute_with_retry_with_zero_retries_still_attempts_once() {
    let store = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store);

    let attempts = repo
        .execute_with_retry::<Counter, AddValue>(&"c1".to_string(), &AddValue { amount: 1 }, &(), 0)
        .await
        .unwrap();

    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn optimistic_execute_with_retry_surfaces_non_concurrency_errors() {
    let store = inmemory::Store::new(JsonCodec);
    let mut repo = Repository::new(store);

    let err = repo
        .execute_with_retry::<Counter, AddValue>(&"c1".to_string(), &AddValue { amount: 0 }, &(), 3)
        .await
        .unwrap_err();

    assert!(matches!(err, OptimisticCommandError::Aggregate(_)));
}

#[derive(Debug)]
struct TrackingSnapshotStore {
    load_called: AtomicBool,
}

impl TrackingSnapshotStore {
    const fn new() -> Self {
        Self {
            load_called: AtomicBool::new(false),
        }
    }

    fn load_called(&self) -> bool {
        self.load_called.load(Ordering::Relaxed)
    }
}

impl SnapshotStore for TrackingSnapshotStore {
    type Id = String;
    type Position = u64;
    type Error = Infallible;

    fn load<'a>(
        &'a self,
        _: &'a str,
        _: &'a Self::Id,
    ) -> impl std::future::Future<Output = Result<Option<Snapshot<Self::Position>>, Self::Error>>
    + Send
    + 'a {
        self.load_called.store(true, Ordering::Relaxed);
        std::future::ready(Ok(None))
    }

    fn offer_snapshot<'a, CE, Create>(
        &'a mut self,
        _: &'a str,
        _: &'a Self::Id,
        _: u64,
        _: Create,
    ) -> impl std::future::Future<
        Output = Result<SnapshotOffer, OfferSnapshotError<Self::Error, CE>>,
    > + Send
    + 'a
    where
        CE: std::error::Error + Send + Sync + 'static,
        Create: FnOnce() -> Result<Snapshot<Self::Position>, CE> + 'a,
    {
        std::future::ready(Ok(SnapshotOffer::Declined))
    }
}

#[tokio::test]
async fn aggregate_builder_load_consults_snapshot_store() {
    let store = inmemory::Store::<String, _, ()>::new(JsonCodec);
    let snapshots = TrackingSnapshotStore::new();
    let repo = Repository::new(store).with_snapshots(snapshots);

    let counter: Counter = repo
        .aggregate_builder()
        .load(&"c1".to_string())
        .await
        .unwrap();

    assert_eq!(counter.value, 0);
    assert!(repo.snapshot_store().load_called());
}
