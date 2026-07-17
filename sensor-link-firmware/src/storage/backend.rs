#[cfg(any(test, feature = "use-std"))]
mod in_memory;

#[cfg(any(test, feature = "use-std"))]
pub use in_memory::InMemoryFlash;

#[cfg(test)]
pub use in_memory::MockError;
