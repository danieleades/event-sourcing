//! Serialization strategy abstraction.
//!
//! `Codec` converts domain events to bytes and back. `SerializableEvent` and
//! `ProjectionEvent` are implemented by aggregate event enums to bridge the gap
//! between domain variants and stored representations.

use thiserror::Error;

/// Error returned when deserializing a stored event fails.
#[derive(Debug, Error)]
pub enum EventDecodeError<CodecError> {
    /// The event kind was not recognized by this event enum.
    #[error("unknown event kind `{kind}`, expected one of {expected:?}")]
    UnknownKind {
        /// The unrecognized event kind string.
        kind: String,
        /// The list of event kinds this enum can handle.
        expected: &'static [&'static str],
    },
    /// The codec failed to deserialize the event data.
    #[error("codec error: {0}")]
    Codec(#[source] CodecError),
}

/// Serialisation strategy used by event stores.
// ANCHOR: codec_trait
pub trait Codec {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Serialize a value for persistence.
    ///
    /// # Errors
    ///
    /// Returns an error from the codec if the value cannot be serialized.
    fn serialize<T>(&self, value: &T) -> Result<Vec<u8>, Self::Error>
    where
        T: serde::Serialize;

    /// Deserialize a value from stored bytes.
    ///
    /// # Errors
    ///
    /// Returns an error from the codec if the bytes cannot be decoded.
    fn deserialize<T>(&self, data: &[u8]) -> Result<T, Self::Error>
    where
        T: serde::de::DeserializeOwned;
}
// ANCHOR_END: codec_trait

/// Trait for event sum types that can serialize themselves for persistence.
///
/// Implemented by aggregate event enums to serialize variants for persistence.
///
/// The `Aggregate` derive macro generates this automatically; hand-written
/// enums can implement it directly if preferred. The generic `M` parameter
/// allows passing through custom metadata types supplied by the store.
pub trait SerializableEvent {
    /// Serialize this event to persistable form with generic metadata.
    ///
    /// # Errors
    ///
    /// Returns a codec error if serialization fails.
    fn to_persistable<C: Codec, M>(
        self,
        codec: &C,
        metadata: M,
    ) -> Result<crate::store::PersistableEvent<M>, C::Error>;
}

/// Trait for event sum types that can deserialize themselves from stored
/// events.
///
/// Implemented by event enums to deserialize stored events.
///
/// Deriving [`Aggregate`](crate::Aggregate) includes a `ProjectionEvent`
/// implementation for the generated sum type. Custom enums can opt in manually
/// using the pattern illustrated below.
pub trait ProjectionEvent: Sized {
    /// The list of event kinds this sum type can deserialize.
    ///
    /// This data is generated automatically when using the derive macros.
    const EVENT_KINDS: &'static [&'static str];

    /// Deserialize an event from stored representation.
    ///
    /// # Errors
    ///
    /// Returns [`EventDecodeError::UnknownKind`] if the event kind is not
    /// recognized, or [`EventDecodeError::Codec`] if deserialization fails.
    fn from_stored<C: Codec>(
        kind: &str,
        data: &[u8],
        codec: &C,
    ) -> Result<Self, EventDecodeError<C::Error>>;
}
