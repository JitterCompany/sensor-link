//! Drain management for timeout operations.
//!
//! This module defines traits and functionality for managing buffer drain operations
//! when timeouts occur in the dispatch system. When receive operations timeout, buffers
//! need to be systematically drained to ensure no data is lost.
//!
//! # Purpose
//!
//! The drain management system ensures that when communication timeouts occur:
//! - All buffers with accumulated data are processed
//! - Data is serialized and dispatched before being discarded
//! - The draining process can be resumed across multiple async iterations
//! - System maintains responsiveness during drain operations
//!
//! # Drain Process
//!
//! 1. **Initiation**: A timeout triggers the start of the drain process
//! 2. **Sequential Processing**: Buffers are processed one by one
//! 3. **State Tracking**: The drain state persists across async boundaries
//! 4. **Completion**: Drain mode ends when all buffers are processed
//!
//! # Design
//!
//! The [`DrainManager`] trait provides a common interface for implementing
//! timeout-based draining across different buffer types and configurations.

/// Trait for managing drain operations when timeouts occur
pub trait DrainManager {
    type Item;

    /// Start the drain process
    fn start_drain(&mut self);

    /// Continue draining, returning the next available item
    fn continue_drain(&mut self) -> Option<Self::Item>;

    /// Check if currently in drain mode
    fn is_draining(&self) -> bool;
}
