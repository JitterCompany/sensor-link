use crate::{logic::dispatch::confirmable::Confirmable, pool::MappedAllocator};

use super::*;

/// Pending data to be sent to the network (if any)
pub struct Pending<A: MappedAllocator> {
    allocator: A,
    data: Option<Confirmable<A::Output>>,
}

pub struct PendWriter<'w, A: MappedAllocator> {
    pending: &'w mut Pending<A>,
}

impl<'w, A: MappedAllocator> PendWriter<'w, A> {
    pub fn write(&mut self, data: A::Input, confirm_handle: ConfirmHandle) {
        self.pending.inner_write(data, Some(confirm_handle));
    }
}

pub struct PendReader<'r, A: MappedAllocator> {
    pending: &'r mut Pending<A>,
}

impl<'r, A: MappedAllocator> PendReader<'r, A> {
    /// Consume the pending item: item is no longer pending.
    ///
    /// The consumer is responsible for confirming the item.
    /// If the consumer drops the item without confirming,
    /// a retry will be triggered if possible.
    pub fn consume(self) -> Confirmable<A::Output> {
        // Safety: PendReader is only created if data is Some
        // and has exclusive (&mut) access to the pending item
        self.pending.data.take().unwrap()
    }
}

impl<A: MappedAllocator> Pending<A> {
    /// Try to write a new pending item.
    ///
    /// Only returns a writer if nothing is pending yet
    pub fn try_set(&mut self) -> Option<PendWriter<'_, A>> {
        if self.is_pending() {
            None
        } else {
            Some(PendWriter { pending: self })
        }
    }

    pub fn try_read(&mut self) -> Option<PendReader<'_, A>> {
        if self.data.is_some() {
            Some(PendReader { pending: self })
        } else {
            None
        }
    }

    /// Initialize a new, empty pending item
    pub fn none(allocator: A) -> Self {
        Self {
            allocator,
            data: None,
        }
    }

    /// Check if there is any pending data
    pub fn is_pending(&self) -> bool {
        self.data.is_some()
    }

    /// Force write new pending data. Drops old pending data if any
    pub fn overwrite(&mut self, data: A::Input) {
        // no confirm handle for retries: this data is permanently lost.
        // note that this can only happen if that data was previously set via
        // `overwrite()`, i.e. if the store failed to store that data.
        if let Some(data) = &self.data {
            if !data.can_be_retried() {
                // TODO how can we keep track if this happens in production without triggering an avalance of events?
                // maybe track 'store health' stats?
                log::error!(target: "Dispatch", "Data loss: permanently dropping pending data!");
            }
        }
        // Note: this drops confirm handle (if any) for previous data: store should retry eventually..
        self.inner_write(data, None);
    }

    /// (internal API: see `overwrite` or `try_set`)
    fn inner_write(&mut self, data: A::Input, confirm_handle: Option<ConfirmHandle>) {
        match self.allocator.alloc(data) {
            Ok(data) => {
                self.data = Some(Confirmable::new(data, confirm_handle));
            }
            // Should never happen: pool should be large enough to fit maximum amount of instances
            Err(_data) => {
                debug_assert!(false, "Alloc failed: pool too small!");
                log::error!(target: "Dispatch", "Alloc failed!");
            }
        }
    }
}
