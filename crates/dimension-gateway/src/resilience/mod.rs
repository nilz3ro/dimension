//! Resilience and flow control components.
//!
//! Provides concurrency limiting, dynamic resizing, and other
//! protective mechanisms for the gateway.

pub mod concurrency;

pub use concurrency::ConcurrencyController;
