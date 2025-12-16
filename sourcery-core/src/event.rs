//! Domain event marker.
//!
//! `DomainEvent` is the lightweight trait every concrete event struct
//! implements. It intentionally avoids persistence concerns; serialization is
//! handled by higher-level traits in the codec module.
use serde::{Serialize, de::DeserializeOwned};

/// Marker trait for events that can be persisted by the event store.
///
/// Each event carries a unique [`Self::KIND`] identifier so the repository can
/// route stored bytes back to the correct type when rebuilding aggregates or
/// projections.
///
/// Most projects implement this trait by hand, but the `#[derive(Aggregate)]`
/// macro generates it automatically for the aggregate event enums it creates.
pub trait DomainEvent: Serialize + DeserializeOwned {
    const KIND: &'static str;
}
