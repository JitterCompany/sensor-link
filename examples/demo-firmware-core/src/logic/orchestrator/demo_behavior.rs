//! Application behavior of the demo device

use crate::logic::orchestrator::application_behavior::{ApplicationBehavior, Store};

#[derive(Debug, Clone)]
pub struct DemoBehavior<S> {
    store: S,
}

impl<S> DemoBehavior<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: Store> ApplicationBehavior for DemoBehavior<S> {
    type Store = S;

    fn store(&mut self) -> &mut S {
        &mut self.store
    }
}
