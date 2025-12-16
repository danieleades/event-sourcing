pub mod aggregate;
pub mod codec;
pub mod concurrency;
pub mod event;
pub mod projection;
pub mod repository;
pub mod snapshot;
pub mod store;

// Test utilities module: public when feature enabled, internal for crate tests
#[cfg(feature = "test-util")]
pub mod test;

#[cfg(all(test, not(feature = "test-util")))]
pub(crate) mod test;
