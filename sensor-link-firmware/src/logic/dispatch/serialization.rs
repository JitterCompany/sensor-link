//! Serialization traits and interfaces for latency-controlled data processing.
//!
//! This module defines the core serialization interface for the dispatch system,
//! focusing on latency control and timeout management. It provides abstractions
//! that allow different buffer implementations to integrate with the dispatch
//! architecture while maintaining consistent latency characteristics.
//!
//! # Key Concepts
//!
//! ## Latency Control
//!
//! The serialization interface is designed around latency control rather than
//! just data transformation. This allows the dispatch system to:
//! - Set maximum acceptable delays for data processing
//! - Configure buffer timeouts to prevent data staleness
//! - Maintain predictable system responsiveness
//!
//! ## Asynchronous Integration
//!
//! All serialization operations are async-aware, enabling:
//! - Non-blocking buffer operations
//! - Timeout-based flow control
//! - Integration with async task scheduling
//!
//! # Traits
//!
//! - [`LatencyControlledSerializer`]: Main interface for buffer implementations
//!   that need to participate in the dispatch system with latency guarantees

use crate::serialize::SerializedSendable;
use sensor_link_protocol::Topic;

/// Trait for serializers that can control latency through timeouts.
///
/// Provides two levels of timeout control:
/// - **Receive Timeout**: How long to wait for new data before triggering drain operations
/// - **Buffer Timeout**: How long data can remain in buffers before forced serialization
///
/// Implementations must respect timeout settings and handle timeout conditions
/// gracefully without data loss.
///
/// # Type Parameters
///
/// * `MAX_OUTPUT_SIZE`: Maximum size of the serialized output in bytes
///
/// # Associated Types
///
/// * `Topic`: the wire topic the serialized output is addressed to. Generic over
///   [`sensor_link_protocol::Topic`] so the buffering machinery stays sensor-agnostic;
///   an implementation pins this to its concrete device topic type.
pub trait LatencyControlledSerializer<const MAX_OUTPUT_SIZE: usize> {
    type Error: core::fmt::Debug;
    type Topic: Topic;

    /// Retrieves the next available serialized packet.
    ///
    /// Waits for new data up to the configured receive timeout, processes available
    /// data through buffers, and handles timeout conditions by draining accumulated data.
    ///
    /// Returns `Some(packet)` when data is available, `None` when no more data.
    async fn next_packet(
        &mut self,
    ) -> Result<Option<SerializedSendable<MAX_OUTPUT_SIZE, Self::Topic>>, Self::Error>;
    /// Sets the receive operation timeout in milliseconds.
    ///
    /// Controls how long to wait for new data before processing accumulated buffer contents.
    /// Shorter timeouts increase responsiveness, longer timeouts improve batching efficiency.
    fn set_timeout(&mut self, timeout_ms: u32);

    /// Sets the buffer timeout in milliseconds.
    ///
    /// Controls how long data can remain in buffers before forced serialization,
    /// preventing data from becoming stale in low-throughput scenarios.
    fn set_buffer_timeout(&mut self, timeout_ms: u32);
}
