#![doc = include_str!("../README.md")]

mod aggregate;
mod codec;
mod event;
mod projection;
mod repository;
mod snapshot;
mod store;

pub use aggregate::{Aggregate, AggregateBuilder, Apply, Handle};
pub use codec::{Codec, ProjectionEvent, SerializableEvent};
pub use event::DomainEvent;
pub use projection::{ApplyProjection, Projection, ProjectionBuilder, ProjectionError};
pub use repository::{CommandError, Repository};
pub use snapshot::{InMemorySnapshotStore, NoSnapshots, Snapshot, SnapshotStore};
pub use store::{
    EventFilter, EventStore, InMemoryEventStore, JsonCodec, LoadEventsResult, PersistableEvent,
    StoredEvent, Transaction,
};

#[cfg(feature = "test-util")]
pub mod test;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::HashMap;

    // ============================================================================
    // Test Fixtures
    // ============================================================================

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
    struct ValueAdded {
        amount: i32,
    }

    impl DomainEvent for ValueAdded {
        const KIND: &'static str = "value-added";
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
    struct ValueSubtracted {
        amount: i32,
    }

    impl DomainEvent for ValueSubtracted {
        const KIND: &'static str = "value-subtracted";
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum CounterEvent {
        Added(ValueAdded),
        Subtracted(ValueSubtracted),
    }

    impl SerializableEvent for CounterEvent {
        fn to_persistable<C: crate::Codec, M>(
            self,
            codec: &C,
            metadata: M,
        ) -> Result<PersistableEvent<M>, C::Error> {
            match self {
                Self::Added(e) => Ok(PersistableEvent {
                    kind: ValueAdded::KIND.to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
                Self::Subtracted(e) => Ok(PersistableEvent {
                    kind: ValueSubtracted::KIND.to_string(),
                    data: codec.serialize(&e)?,
                    metadata,
                }),
            }
        }
    }

    impl ProjectionEvent for CounterEvent {
        const EVENT_KINDS: &'static [&'static str] = &[ValueAdded::KIND, ValueSubtracted::KIND];

        fn from_stored<C: crate::Codec>(
            kind: &str,
            data: &[u8],
            codec: &C,
        ) -> Result<Self, C::Error> {
            match kind {
                "value-added" => Ok(Self::Added(codec.deserialize(data)?)),
                "value-subtracted" => Ok(Self::Subtracted(codec.deserialize(data)?)),
                _ => panic!("Unknown event kind: {kind}"),
            }
        }
    }

    #[derive(Debug, Default, Serialize, serde::Deserialize)]
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

    impl Apply<ValueAdded> for Counter {
        fn apply(&mut self, event: &ValueAdded) {
            self.value += event.amount;
        }
    }

    impl Apply<ValueSubtracted> for Counter {
        fn apply(&mut self, event: &ValueSubtracted) {
            self.value -= event.amount;
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

    #[derive(Default)]
    struct CounterProjection {
        totals: HashMap<String, i32>,
    }

    impl Projection for CounterProjection {
        type Metadata = ();
    }

    impl ApplyProjection<ValueAdded, ()> for CounterProjection {
        fn apply_projection(&mut self, aggregate_id: &str, event: &ValueAdded, _metadata: &()) {
            *self.totals.entry(aggregate_id.to_string()).or_default() += event.amount;
        }
    }

    impl ApplyProjection<ValueSubtracted, ()> for CounterProjection {
        fn apply_projection(
            &mut self,
            aggregate_id: &str,
            event: &ValueSubtracted,
            _metadata: &(),
        ) {
            *self.totals.entry(aggregate_id.to_string()).or_default() -= event.amount;
        }
    }

    // ============================================================================
    // EventFilter Tests
    // ============================================================================

    mod event_filter {
        use super::*;

        #[test]
        fn for_event_creates_unrestricted_filter() {
            let filter = EventFilter::for_event("my-event");

            assert_eq!(filter.event_kind, "my-event");
            assert_eq!(filter.aggregate_kind, None);
            assert_eq!(filter.aggregate_id, None);
        }

        #[test]
        fn for_aggregate_creates_restricted_filter() {
            let filter = EventFilter::for_aggregate("my-event", "my-aggregate", "123");

            assert_eq!(filter.event_kind, "my-event");
            assert_eq!(filter.aggregate_kind, Some("my-aggregate".to_string()));
            assert_eq!(filter.aggregate_id, Some("123".to_string()));
        }

        #[test]
        fn filters_are_equal_when_fields_match() {
            let filter1 = EventFilter::for_aggregate("event", "agg", "id");
            let filter2 = EventFilter::for_aggregate("event", "agg", "id");

            assert_eq!(filter1, filter2);
        }
    }

    // ============================================================================
    // JsonCodec Tests
    // ============================================================================

    mod json_codec {
        use super::*;

        #[test]
        fn roundtrip_serialization() {
            let codec = JsonCodec;
            let event = ValueAdded { amount: 42 };

            let bytes = codec.serialize(&event).unwrap();
            let decoded: ValueAdded = codec.deserialize(&bytes).unwrap();

            assert_eq!(decoded, event);
        }

        #[test]
        fn deserialize_invalid_json_returns_error() {
            let codec = JsonCodec;
            let invalid_bytes = b"not valid json";

            let result: Result<ValueAdded, _> = codec.deserialize(invalid_bytes);

            assert!(result.is_err());
        }

        #[test]
        fn deserialize_wrong_type_returns_error() {
            let codec = JsonCodec;
            let wrong_json = br#"{"wrong_field": 123}"#;

            let result: Result<ValueAdded, _> = codec.deserialize(wrong_json);

            assert!(result.is_err());
        }
    }

    // ============================================================================
    // InMemoryEventStore Tests
    // ============================================================================

    mod in_memory_event_store {
        use super::*;

        fn create_store() -> InMemoryEventStore<JsonCodec, ()> {
            InMemoryEventStore::new(JsonCodec)
        }

        fn append_event(
            store: &mut InMemoryEventStore<JsonCodec, ()>,
            aggregate_kind: &str,
            aggregate_id: &str,
            event_kind: &str,
            data: &[u8],
        ) {
            let events = vec![PersistableEvent {
                kind: event_kind.to_string(),
                data: data.to_vec(),
                metadata: (),
            }];
            store
                .append_batch(aggregate_kind, aggregate_id, events)
                .unwrap();
        }

        #[test]
        fn append_and_load_single_event() {
            let mut store = create_store();
            let data = br#"{"amount":10}"#;

            append_event(&mut store, "counter", "c1", "value-added", data);

            let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
            let events = store.load_events(&filters).unwrap();

            assert_eq!(events.len(), 1);
            assert_eq!(events[0].aggregate_kind, "counter");
            assert_eq!(events[0].aggregate_id, "c1");
            assert_eq!(events[0].kind, "value-added");
            assert_eq!(events[0].data, data);
        }

        #[test]
        fn append_batch_multiple_events() {
            let mut store = create_store();
            let events = vec![
                PersistableEvent {
                    kind: "value-added".to_string(),
                    data: br#"{"amount":10}"#.to_vec(),
                    metadata: (),
                },
                PersistableEvent {
                    kind: "value-subtracted".to_string(),
                    data: br#"{"amount":5}"#.to_vec(),
                    metadata: (),
                },
            ];

            store.append_batch("counter", "c1", events).unwrap();

            let filters = vec![
                EventFilter::for_aggregate("value-added", "counter", "c1"),
                EventFilter::for_aggregate("value-subtracted", "counter", "c1"),
            ];
            let loaded = store.load_events(&filters).unwrap();

            assert_eq!(loaded.len(), 2);
            assert_eq!(loaded[0].kind, "value-added");
            assert_eq!(loaded[1].kind, "value-subtracted");
        }

        #[test]
        fn load_empty_returns_empty_vec() {
            let store = create_store();
            let filters = vec![EventFilter::for_event("nonexistent")];

            let events = store.load_events(&filters).unwrap();

            assert!(events.is_empty());
        }

        #[test]
        fn load_filters_by_event_kind() {
            let mut store = create_store();
            append_event(&mut store, "counter", "c1", "value-added", b"{}");
            append_event(&mut store, "counter", "c1", "value-subtracted", b"{}");

            let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
            let events = store.load_events(&filters).unwrap();

            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, "value-added");
        }

        #[test]
        fn load_filters_by_aggregate() {
            let mut store = create_store();
            append_event(&mut store, "counter", "c1", "value-added", b"{}");
            append_event(&mut store, "counter", "c2", "value-added", b"{}");

            let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
            let events = store.load_events(&filters).unwrap();

            assert_eq!(events.len(), 1);
            assert_eq!(events[0].aggregate_id, "c1");
        }

        #[test]
        fn global_position_ordering() {
            let mut store = create_store();

            // Add events to different streams
            append_event(&mut store, "counter", "c1", "value-added", b"{}");
            append_event(&mut store, "counter", "c2", "value-added", b"{}");
            append_event(&mut store, "counter", "c1", "value-added", b"{}");

            // Load all value-added events across all counters
            let filters = vec![EventFilter::for_event("value-added")];
            let events = store.load_events(&filters).unwrap();

            assert_eq!(events.len(), 3);
            // Events should be sorted by global position
            assert!(events[0].position < events[1].position);
            assert!(events[1].position < events[2].position);
            // First event should be from c1, second from c2, third from c1
            assert_eq!(events[0].aggregate_id, "c1");
            assert_eq!(events[1].aggregate_id, "c2");
            assert_eq!(events[2].aggregate_id, "c1");
        }

        #[test]
        fn deduplication_when_filters_overlap() {
            let mut store = create_store();
            append_event(&mut store, "counter", "c1", "value-added", b"{}");

            // Create overlapping filters: one specific, one global
            let filters = vec![
                EventFilter::for_aggregate("value-added", "counter", "c1"),
                EventFilter::for_event("value-added"),
            ];
            let events = store.load_events(&filters).unwrap();

            // Should only get one event, not two
            assert_eq!(events.len(), 1);
        }

        #[test]
        fn positions_are_globally_unique() {
            let mut store = create_store();

            append_event(&mut store, "a", "1", "e", b"{}");
            append_event(&mut store, "b", "2", "e", b"{}");
            append_event(&mut store, "a", "1", "e", b"{}");

            let filters = vec![EventFilter::for_event("e")];
            let events = store.load_events(&filters).unwrap();

            let positions: Vec<u64> = events.iter().map(|e| e.position).collect();
            assert_eq!(positions, vec![0, 1, 2]);
        }
    }

    // ============================================================================
    // Transaction Tests
    // ============================================================================

    mod transaction {
        use super::*;

        #[test]
        fn commit_persists_events() {
            let mut store: InMemoryEventStore<JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);

            {
                let mut tx = store.begin("counter", "c1");
                tx.append(CounterEvent::Added(ValueAdded { amount: 10 }), ())
                    .unwrap();
                tx.commit().unwrap();
            }

            let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
            let events = store.load_events(&filters).unwrap();
            assert_eq!(events.len(), 1);
        }

        #[test]
        fn drop_discards_uncommitted_events() {
            let mut store: InMemoryEventStore<JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);

            {
                let mut tx = store.begin("counter", "c1");
                tx.append(CounterEvent::Added(ValueAdded { amount: 10 }), ())
                    .unwrap();
                // tx dropped without commit
            }

            let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
            let events = store.load_events(&filters).unwrap();
            assert!(events.is_empty());
        }

        #[test]
        fn append_multiple_events_in_transaction() {
            let mut store: InMemoryEventStore<JsonCodec, ()> = InMemoryEventStore::new(JsonCodec);

            {
                let mut tx = store.begin("counter", "c1");
                tx.append(CounterEvent::Added(ValueAdded { amount: 10 }), ())
                    .unwrap();
                tx.append(CounterEvent::Subtracted(ValueSubtracted { amount: 5 }), ())
                    .unwrap();
                tx.commit().unwrap();
            }

            let filters = vec![
                EventFilter::for_aggregate("value-added", "counter", "c1"),
                EventFilter::for_aggregate("value-subtracted", "counter", "c1"),
            ];
            let events = store.load_events(&filters).unwrap();
            assert_eq!(events.len(), 2);
        }
    }

    // ============================================================================
    // Repository Tests
    // ============================================================================

    mod repository {
        use super::*;

        struct NoOpCommand;

        impl Handle<NoOpCommand> for Counter {
            fn handle(&self, _: &NoOpCommand) -> Result<Vec<Self::Event>, Self::Error> {
                Ok(vec![]) // No events produced
            }
        }

        fn create_repository() -> Repository<InMemoryEventStore<JsonCodec, ()>> {
            Repository::new(InMemoryEventStore::new(JsonCodec))
        }

        #[test]
        fn execute_command_success() {
            let mut repo = create_repository();

            let result = repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: 10 },
                &(),
            );

            assert!(result.is_ok());

            // Verify events were persisted
            let filters = vec![EventFilter::for_aggregate("value-added", "counter", "c1")];
            let events = repo.event_store().load_events(&filters).unwrap();
            assert_eq!(events.len(), 1);
        }

        #[test]
        fn execute_command_aggregate_error() {
            let mut repo = create_repository();

            let result = repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: -5 }, // Invalid: negative amount
                &(),
            );

            assert!(matches!(result, Err(CommandError::Aggregate(_))));
        }

        #[test]
        fn execute_command_empty_events_skips_persistence() {
            let mut repo = create_repository();

            // First, add some value
            repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: 10 },
                &(),
            )
            .unwrap();

            let result =
                repo.execute_command::<Counter, NoOpCommand>(&"c1".to_string(), &NoOpCommand, &());

            assert!(result.is_ok());
        }

        #[test]
        fn load_aggregate_rebuilds_state() {
            let mut repo = create_repository();

            // Execute multiple commands
            repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: 10 },
                &(),
            )
            .unwrap();
            repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: 5 },
                &(),
            )
            .unwrap();

            // Load aggregate and verify state
            let counter: Counter = repo.aggregate_builder().load(&"c1".to_string()).unwrap();

            assert_eq!(counter.value, 15);
        }

        #[test]
        fn load_aggregate_for_nonexistent_returns_default() {
            let repo = create_repository();

            let counter: Counter = repo
                .aggregate_builder()
                .load(&"nonexistent".to_string())
                .unwrap();

            assert_eq!(counter.value, 0);
        }

        #[test]
        fn build_projection_applies_events() {
            let mut repo = create_repository();

            // Add events to multiple aggregates
            repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: 10 },
                &(),
            )
            .unwrap();
            repo.execute_command::<Counter, AddValue>(
                &"c2".to_string(),
                &AddValue { amount: 20 },
                &(),
            )
            .unwrap();

            // Build projection
            let projection: CounterProjection = repo
                .build_projection()
                .event::<ValueAdded>()
                .load()
                .unwrap();

            assert_eq!(projection.totals.get("c1"), Some(&10));
            assert_eq!(projection.totals.get("c2"), Some(&20));
        }

        #[test]
        fn command_validation_uses_current_state() {
            let mut repo = create_repository();

            // Add 10
            repo.execute_command::<Counter, AddValue>(
                &"c1".to_string(),
                &AddValue { amount: 10 },
                &(),
            )
            .unwrap();

            // Try to subtract 15 (should fail - insufficient value)
            let result = repo.execute_command::<Counter, SubtractValue>(
                &"c1".to_string(),
                &SubtractValue { amount: 15 },
                &(),
            );

            assert!(matches!(result, Err(CommandError::Aggregate(_))));

            // Subtract 5 (should succeed)
            let result = repo.execute_command::<Counter, SubtractValue>(
                &"c1".to_string(),
                &SubtractValue { amount: 5 },
                &(),
            );

            assert!(result.is_ok());
        }
    }

    // ============================================================================
    // Error Type Tests
    // ============================================================================

    mod errors {
        use super::*;
        use std::error::Error;
        use std::io;

        #[test]
        fn projection_error_display_store() {
            let error: ProjectionError<io::Error, io::Error> =
                ProjectionError::Store(io::Error::new(io::ErrorKind::NotFound, "not found"));

            let msg = format!("{error}");
            assert!(msg.contains("failed to load events"));
        }

        #[test]
        fn projection_error_display_codec() {
            let error: ProjectionError<io::Error, io::Error> =
                ProjectionError::Codec(io::Error::new(io::ErrorKind::InvalidData, "bad data"));

            let msg = format!("{error}");
            assert!(msg.contains("failed to decode event"));
        }

        #[test]
        fn command_error_display_aggregate() {
            let error: CommandError<String, io::Error, io::Error, io::Error> =
                CommandError::Aggregate("invalid state".to_string());

            let msg = format!("{error}");
            assert!(msg.contains("aggregate rejected command"));
        }

        #[test]
        fn projection_error_source() {
            let inner = io::Error::new(io::ErrorKind::NotFound, "inner error");
            let error: ProjectionError<io::Error, io::Error> = ProjectionError::Store(inner);

            assert!(error.source().is_some());
        }

        #[test]
        fn command_error_aggregate_has_no_source() {
            let error: CommandError<String, io::Error, io::Error, io::Error> =
                CommandError::Aggregate("test".to_string());

            assert!(error.source().is_none());
        }

        #[test]
        fn command_error_store_has_source() {
            let inner = io::Error::other("store error");
            let error: CommandError<String, io::Error, io::Error, io::Error> =
                CommandError::Store(inner);

            assert!(error.source().is_some());
        }
    }

    // ============================================================================
    // PersistableEvent Tests
    // ============================================================================

    mod persistable_event {
        use super::*;

        #[test]
        fn serializable_event_produces_persistable() {
            let event = CounterEvent::Added(ValueAdded { amount: 10 });
            let codec = JsonCodec;

            let persistable = event.to_persistable(&codec, ()).unwrap();

            assert_eq!(persistable.kind, "value-added");
            assert!(!persistable.data.is_empty());
        }
    }
}
