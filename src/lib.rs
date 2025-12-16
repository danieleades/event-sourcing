#![doc = include_str!("../README.md")]

pub use sourcery_core::{aggregate, codec, concurrency, projection, repository, snapshot};
pub use sourcery_core::{
    aggregate::Aggregate, aggregate::Apply, aggregate::Handle, event::DomainEvent,
    projection::ApplyProjection, projection::Projection, repository::Repository,
};

// Re-export proc macro derives so consumers only depend on `sourcery`.
pub use sourcery_macros::Aggregate;

#[cfg(feature = "test-util")]
pub use sourcery_core::test;

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
