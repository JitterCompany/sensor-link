use serde::Serialize;

/// Marker trait for types that identify a sensor data channel or data type.
///
/// Implement this in the application crate for the concrete channel/type enums.
/// sensor-core uses it as a bound so its store traits and data structures
/// remain independent of any specific sensor model.
pub trait DataKind:
    Clone
    + Copy
    + PartialEq
    + Eq
    + std::hash::Hash
    + std::fmt::Debug
    + std::fmt::Display
    + Serialize
    + Send
    + Sync
    + 'static
{
    /// Whether aggregated (downsampled) time-series views are built for this kind.
    fn downsampling(&self) -> bool;
}
