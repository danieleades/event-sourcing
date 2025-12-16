#![doc = include_str!("../README.md")]

pub use event_sourcing_core::{aggregate, codec, concurrency, projection, repository, snapshot};
pub use event_sourcing_core::{
    aggregate::Aggregate, aggregate::Apply, aggregate::Handle, event::DomainEvent,
    projection::ApplyProjection, projection::Projection, repository::Repository,
};

// Re-export proc macro derives so consumers only depend on `event-sourcing`.
pub use event_sourcing_macros::Aggregate;

#[cfg(feature = "test-util")]
pub use event_sourcing_core::test;

pub mod store {

    pub use event_sourcing_core::store::{
        EventFilter, EventStore, JsonCodec, PersistableEvent, StoredEvent, Transaction,
    };

    #[cfg(feature = "postgres")]
    #[cfg_attr(docsrs, doc(cfg(feature = "postgres")))]
    pub mod postgres {
        pub use event_sourcing_postgres::{Store, Error};
    }

    pub use event_sourcing_core::store::inmemory;
}
