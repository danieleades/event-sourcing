#![doc = include_str!("../README.md")]

#[cfg(feature = "test-util")]
pub use sourcery_core::test;
pub use sourcery_core::{
    aggregate,
    aggregate::{Aggregate, Apply, Handle},
    codec, concurrency,
    event::DomainEvent,
    projection,
    projection::{ApplyProjection, Projection},
    repository,
    repository::Repository,
    snapshot,
};
// Re-export proc macro derives so consumers only depend on `sourcery`.
pub use sourcery_macros::Aggregate;

pub mod store {

    pub use sourcery_core::store::{
        EventFilter, EventStore, JsonCodec, PersistableEvent, StoredEvent, Transaction,
    };

    #[cfg(feature = "postgres")]
    #[cfg_attr(docsrs, doc(cfg(feature = "postgres")))]
    pub mod postgres {
        pub use sourcery_postgres::{Error, Store};
    }

    pub use sourcery_core::store::inmemory;
}
