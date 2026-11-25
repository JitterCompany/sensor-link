//! Wrapper implementations for demo-protocol serializers.
//!
//! This module provides adapter wrappers that bridge demo-protocol serializers
//! with the dispatch system's [`BufferSerializer`] trait. These wrappers enable
//! the existing sensor-specific serializers to work seamlessly with the new
//! sensor-agnostic buffer management system.
//!
//! # Purpose
//!
//! Each data stream has a specialized serializer with its own API. These
//! wrappers standardize the interface so all serializers can be used
//! interchangeably within the buffer management system.
//!
//! # Design Pattern
//!
//! Each wrapper implements the [`BufferSerializer`] trait by:
//! 1. Accepting standardized [`UniformSamples`] data
//! 2. Handling optional channel information as needed
//! 3. Delegating to the appropriate demo-protocol serializer
//! 4. Returning standardized [`SerializedSendable`] results
//!
//! # Available Wrappers
//!
//! - [`DemoDataSerializerWrapper`]: Wraps [`DemoDataSerializer`] for the demo data stream

use demo_protocol::{DemoData, TopicFromDevice, NUM_CH_DEMO};
use sensor_link_firmware::logic::serializer::SerializeError;

use crate::SerializedSendable;

use super::{buffer::BufferSerializer, demo_data::DemoDataSerializer};

/// Wrapper for DemoDataSerializer to implement BufferSerializer trait.
///
/// Carries the channel it serializes for, keeping channel knowledge out of the generic buffer layer.
pub struct DemoDataSerializerWrapper;

impl<const MAX_INPUT_SIZE: usize, const M: usize> BufferSerializer<NUM_CH_DEMO, MAX_INPUT_SIZE, M>
    for DemoDataSerializerWrapper
{
    type Error = SerializeError;
    type Topic = TopicFromDevice;

    fn serialize(
        &self,
        samples: &DemoData<MAX_INPUT_SIZE>,
    ) -> Result<SerializedSendable<M>, Self::Error> {
        DemoDataSerializer::<M>::serialize(samples)
    }
}
