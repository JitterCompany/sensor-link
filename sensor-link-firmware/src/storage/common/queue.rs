//! Persistent Queue bookkeeping logic
//!
//! In-memory bookkeeping to track which queue elements are available.
//! Intended for internal use in persistent_store
use core::fmt;

use embassy_sync;

/// Sequence number: unique identifier for an item in the queue
///
/// Sequence numbers are unique *per queue* and *at least while the item exists in the queue*.
/// They are always sequential and wrap to zero on overflow. After an item is read (confirmed)
/// from the queue, its sequence number will eventually be reused.
pub type SeqNo = u32;

#[derive(Debug, Clone, Copy)]
pub enum Error {
    QueueFull,
}

// required for embassy_sync::channel 'cross-platform'.
// Alternate option: remove std feature, make channel a generic parameter where needed
#[cfg(any(test, feature = "use-std"))]
mod std_mutex {
    use core::ops::DerefMut;

    use embassy_sync::blocking_mutex::raw::RawMutex;

    pub struct MyMutex {
        inner: std::sync::Mutex<u8>,
    }

    impl MyMutex {
        const fn new() -> Self {
            Self {
                inner: std::sync::Mutex::new(0),
            }
        }
    }

    unsafe impl RawMutex for MyMutex {
        const INIT: Self = Self::new();

        fn lock<R>(&self, f: impl FnOnce() -> R) -> R {
            let mut guard = self.inner.lock().unwrap();

            let result = f();

            let _ = guard.deref_mut();
            drop(guard);
            result
        }
    }
}

#[cfg(any(test, feature = "use-std"))]
use std_mutex::MyMutex;

#[cfg(not(any(test, feature = "use-std")))]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as MyMutex;

#[derive(Debug)]
struct Confirm {
    id: SeqNo,
    success: bool,
}

/// Channel used to send confirmations from [ConfirmHandle] to the [Queue]
///
/// One channel must be created and given to the Queue. This is only a separate
/// object because of lifetime requirements (it must strictly outlive the queue)
pub struct ConfirmChannel<const MAX_PEEKS: usize>(
    embassy_sync::channel::Channel<MyMutex, Confirm, MAX_PEEKS>,
);

impl<const MAX_PEEKS: usize> ConfirmChannel<MAX_PEEKS> {
    pub const fn new() -> Self {
        Self(embassy_sync::channel::Channel::new())
    }
}

impl<'ch, const _N: usize> Drop for ConfirmHandle<'ch, _N> {
    fn drop(&mut self) {
        self.try_abort()
    }
}

/// Confirm or abort a peek
///
/// When this handle is dropped (explicitly or as it goes out of scope),
/// the read is marked as unsuccesfull and will be retried later.
/// To mark a read as succesfull, see [confirm()](ConfirmHandle::confirm())
pub struct ConfirmHandle<'ch, const MAX_PEEKS: usize> {
    id: SeqNo,
    sender: Option<embassy_sync::channel::Sender<'ch, MyMutex, Confirm, MAX_PEEKS>>,
    //sender: SendChannel,
}
impl<'ch, const MAX_PEEKS: usize> fmt::Debug for ConfirmHandle<'ch, MAX_PEEKS> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConfirmHandle {}", self.id)
    }
}

impl<'ch, const MAX_PEEKS: usize> ConfirmHandle<'ch, MAX_PEEKS> {
    /// Create a new confirm handle
    ///
    /// # Safety
    ///
    /// The constructor is private because a 1-1 mapping *must* exist
    /// between the SeqNo and the ConfirmHandle. Otherwise the user could keep
    /// a handle untill the ids have wrapped around and accidentally confirm an
    /// item that was not processed yet.
    fn new(
        id: SeqNo,
        sender: embassy_sync::channel::Sender<'ch, MyMutex, Confirm, MAX_PEEKS>,
    ) -> Self {
        // Note: switching to embassy_sync::channel::DynamicSender would eliminate the MAX_PEEKS generic argument,
        // but that wont work for the mock sensor as tokio requires Send..
        Self {
            id,
            sender: Some(sender),
        }
    }

    /// Sequence number of the item
    ///
    /// Sequentially enqueued items always have successive sequence numbers
    /// (except when wrapping on overflow)
    pub fn seq_no(&self) -> SeqNo {
        self.id
    }

    /// Try to mark the item as aborted (called via [Drop])
    ///
    /// This has no effect if the item has already been confirmed.
    fn try_abort(&mut self) {
        // Take the sender so that there can't be a scenario where the item is aborted twice or aborted + confirmed.
        if let Some(sender) = self.sender.take() {
            sender
                .try_send(Confirm {
                    id: self.id,
                    success: false,
                })
                .ok();
        }
    }

    /// Mark the item as confirmed (safe to delete from storage)
    pub fn confirm(mut self) {
        // Take the sender so that drop won't cause an abort right after confirming
        if let Some(sender) = self.sender.take() {
            sender
                .try_send(Confirm {
                    id: self.id,
                    success: true,
                })
                .ok();
        }
        // Note: self goes out of scope so Drop::drop() is called here.
        // That is why the sender must be repaced with None at this point.
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReadState {
    // Slot available for new reader
    None,

    // Reader busy (should wait for confirm/abort)
    Busy(u32),

    // Read aborted (should retry)
    Abort(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReadBlocked {
    /// Busy: one or more previous reads must be confirmed first
    Busy,

    /// Empty: the queue is empty (reading is blocked untill new items are enqueued)
    Empty,

    /// Should not happen. Bug?
    Unknown,
}

struct PeekQueue<const MAX_PEEKS: usize> {
    read_index_min: u32, // Read lower bound: sequence number before this are read & confirmed
    read_index_next: u32, // Read higher bound: between lower and higher bound are peeked but not confirmed yet

    // Circular buffer of active 'peeks' (seq_no, ReadState)
    reader_slots: [ReadState; MAX_PEEKS],
    slot_index: usize,

    // internal statistics
    stats_n_confirmed: u64,
    stats_n_aborted: u64,
}

impl<const MAX_PEEKS: usize> PeekQueue<MAX_PEEKS> {
    pub fn new(read_index_min: u32) -> Self {
        Self {
            read_index_min,
            read_index_next: read_index_min,

            reader_slots: [ReadState::None; MAX_PEEKS],
            slot_index: (read_index_min % MAX_PEEKS as u32) as usize,

            stats_n_confirmed: 0,
            stats_n_aborted: 0,
        }
    }

    pub(crate) fn min_index(&self) -> u32 {
        self.read_index_min
    }

    /// Bump ahead the read indices
    ///
    /// Independent of the active readers: indices 'below' the new index
    /// are considered obsolete (erased)
    pub(crate) fn bump_min_index(&mut self, min: SeqNo) {
        let skipped = min.wrapping_sub(self.read_index_min);
        let readers_active = self.read_index_next.wrapping_sub(self.read_index_min);
        self.read_index_min = min;
        if readers_active < skipped {
            self.read_index_next = min;
        }
    }

    /// Check if a given `seq_no` is within the valid readable range
    ///
    /// (if not, the underlying data may have been erased already)
    fn is_in_bounds(&self, seq_no: u32) -> bool {
        if seq_no.wrapping_sub(self.min_index()) > MAX_PEEKS as u32 {
            return false;
        }
        if self.read_index_next.wrapping_sub(seq_no) > MAX_PEEKS as u32 {
            return false;
        }

        true
    }

    /// Update peeker state based on incoming `Confirm`
    pub(crate) fn update(&mut self, confirm: Confirm) {
        let slot_index = (confirm.id % (MAX_PEEKS as u32)) as usize;
        let slot = &mut self.reader_slots[slot_index];

        match slot {
            // Slot should be in Busy state with matching seq_no
            ReadState::Busy(seq_no) if *seq_no == confirm.id => {
                let seq_no = *seq_no;
                *slot = match confirm.success {
                    true => {
                        self.stats_n_confirmed = self.stats_n_confirmed.saturating_add(1);
                        ReadState::None
                    }
                    false => {
                        self.stats_n_aborted = self.stats_n_aborted.saturating_add(1);
                        ReadState::Abort(seq_no)
                    }
                }
            }

            // Not busy or seq_no mismatch: this should not be possible, as this indicates
            // a confirmation of a seq_no that was never marked busy (or a duplicate confirmation).
            // Most likely a bug in the upper layer logic. But let's not panic in production..
            state => {
                log::warn!(target: "PeekQueue", "BUG: ignore confirm #{}: {state:?}!", confirm.id);
                debug_assert!(false, "Confirmed while not in expected state!");
            }
        }
    }

    /// Try to find an available reader
    ///
    /// search up to `write_index` (seq_no == write_index means queue empty)
    pub(crate) fn find_reader(&mut self, write_index: u32) -> Result<SeqNo, ReadBlocked> {
        // Usually only one iteration is needed to make progress.
        // More iterations can be required after recovering from aborted reads
        // The worst-case:
        // - just after a retry where the previous slot contained the oldest possible item (MAX_PEEKS-1 ago)
        // - if the queue is empty in combination with the previous point it may take up to MAX_PEEKS to
        //     recognize this (as read_index_min is only incremented one-at-a-time)
        for _ in 0..(2 * MAX_PEEKS) {
            let slot_id = self.slot_index;
            let slot = self.reader_slots[slot_id];
            match slot {
                // Reader still busy in this slot. Try again later untill it is finished
                // don't skip to the next slot to keep the queue as sequential as possible)
                ReadState::Busy(_) => return Err(ReadBlocked::Busy),

                // Empty slot: try to allocate a new read at 'read_index_next'
                ReadState::None => {
                    let alloc_index = self.read_index_next;

                    self.inc_slot_index();

                    // SeqNo N is always in slot (N % MAX_PEEKERS)
                    let slot_seq_no = self.read_index_min
                        - (self.read_index_min % MAX_PEEKS as u32)
                        + slot_id as u32;

                    // Update read_index_min: this index is no longer busy
                    // and may be replaced with a new value
                    // Note: this cannot be done in Self::update as confirms
                    // can be in any order!
                    if slot_seq_no == self.read_index_min && slot_seq_no != alloc_index {
                        self.read_index_min = self.read_index_min.wrapping_add(1);
                    }

                    // Nothing new to read (queue empty)?
                    if alloc_index == write_index {
                        // In case of retries, there are still potentially things to retry
                        if self.min_index() != write_index {
                            // Note: this has side-effect of inc slot_index, where it may end up waiting on a busy slot even though there are still free slots to alloc.
                            // (only happens if queue not full, probably not worth optimizing..)
                            continue;
                        }
                        return Err(ReadBlocked::Empty);
                    }

                    // Each SeqNo has a specific target slot where its peeker/reader state should be saved.
                    // If the current slot is not for the requested SeqNo, skip untill we find the right slot.
                    // (this only happens after recovering from one or more unconfirmed reads)
                    let target_slot = (alloc_index % (MAX_PEEKS as u32)) as usize;
                    if slot_id == target_slot {
                        self.read_index_next = alloc_index + 1;

                        self.reader_slots[slot_id] = ReadState::Busy(alloc_index);
                        return Ok(alloc_index);
                    }
                }

                // Aborted read: retry it if possible
                ReadState::Abort(index) => {
                    // Valid index: mark as busy and return it
                    if self.is_in_bounds(index) {
                        self.inc_slot_index();
                        self.reader_slots[slot_id] = ReadState::Busy(index);
                        return Ok(index);

                    // Invalid index: cannot retry, so mark slot as empty.
                    // (next iteration will try to allocate a read in this slot)
                    } else {
                        log::debug!(target: "PeekQueue", "skip retry of #{slot_id}: not in range");
                        self.reader_slots[slot_id] = ReadState::None;
                    }
                }
            }
        }

        return Err(ReadBlocked::Unknown);
    }

    /// Circular increase to the next slot
    fn inc_slot_index(&mut self) {
        self.slot_index += 1;
        if self.slot_index >= MAX_PEEKS {
            self.slot_index = 0;
        }
    }
}

/// Bookkeeping for a queue that allows multiple cancellable 'peek' reads
///
/// This struct does the bookkeeping part: giving out a handle for each elements
/// and deciding which element to re-issue if it was previously aborted.
/// Note that this does not own any data, as it is intended for use with
/// persistent data (which cannot fit in RAM).
pub struct Queue<'ch, const MAX_PEEKS: usize> {
    // queue capacity. If write_index is further than capacity ahead of read_index_min, this implies that
    // part of the queue has been overwritten.
    capacity: u32,

    // how many elements from the queue are erased when the queue is full and circularly overwritten (see capacity)
    // e.g. the items between `write_index.next_multiple_of(overwrite_granularity) - capacity` and `read_index_min`
    // should be assumed lost and skipped.
    overwrite_granularity: u32,

    write_index: SeqNo, // Sequence number given to the next enqueued item

    // Circular queue of peekers (readers that may confirm/abort)
    readers: PeekQueue<MAX_PEEKS>,

    // Active peeks will send confirm or abort via this channel
    confirm_channel: &'ch ConfirmChannel<MAX_PEEKS>,
}

impl<'ch, const MAX_PEEKS: usize> Queue<'ch, MAX_PEEKS> {
    /// Instantiate a new Queue without circular overwrite support.
    ///
    /// Note that this requires a [ConfirmChannel] which must have a lifetime longer than the Queue
    /// and all its borrows (which probably means static in practice)
    pub fn new(ch: &'ch ConfirmChannel<MAX_PEEKS>) -> Self {
        Self::with_overwrite_support(ch, usize::MAX, 1)
    }

    /// Instantiate a new Queue with overwrite support
    ///
    /// Note that this requires a [ConfirmChannel] which must have a lifetime longer than the Queue
    /// and all its borrows (which probably means static in practice)
    pub fn with_overwrite_support(
        ch: &'ch ConfirmChannel<MAX_PEEKS>,
        capacity: usize,
        overwrite_granularity: usize,
    ) -> Self {
        let capacity = u32::try_from(capacity).unwrap_or(u32::MAX);
        let overwrite_granularity = u32::try_from(overwrite_granularity).unwrap_or(u32::MAX);
        Self {
            confirm_channel: ch,
            capacity,
            overwrite_granularity,

            write_index: 0,
            readers: PeekQueue::new(0),
        }
    }

    /// Restore the read/write state (e.g. from persisten storage).
    ///
    /// The given range is the range of valid sequence numbers. For example (10..199) means that
    /// sequence number 10 up to (excluding) 199 can be read and 199 is the next sequence number
    /// used when new values are pushed to the queue.
    pub fn reinit_with_existing_range(&mut self, range: (SeqNo, SeqNo)) {
        *self = Self {
            confirm_channel: self.confirm_channel,

            capacity: self.capacity,
            overwrite_granularity: self.overwrite_granularity,

            write_index: range.1,
            readers: PeekQueue::new(range.0),
        };
    }

    fn update_reader_slots(&mut self) {
        // Read confirmations from the queue if any and update the status per slot.
        // try_alloc_reader will pick up the new state later.
        while let Ok(confirm) = self.confirm_channel.0.try_receive() {
            self.readers.update(confirm);
        }
    }

    fn try_alloc_reader(&mut self) -> Result<SeqNo, ReadBlocked> {
        self.readers.find_reader(self.write_index)
    }

    /// How many unconfirmed items are left at most.
    ///
    /// This is an upper bound, this is only updated by try_alloc_reader!
    ///
    /// This includes both items that have never been read
    /// and items that have been peeked but not confirmed yet
    fn unconfirmed_count(&self) -> u32 {
        self.write_index.wrapping_sub(self.readers.min_index())
    }

    /// Estimate of the existing range of items
    ///
    /// Note: the lower bound may not be up-to-date untill after
    /// calling [Self::try_alloc_reader] (may be outdated by up to `MAX_PEEKS`)
    pub fn existing_range(&self) -> (u32, u32) {
        (self.readers.min_index(), self.write_index)
    }

    /// Add an item to the queue
    ///
    /// The queue behaves like a FIFO. See [peek_next()](Queue::peek_next()).
    ///
    /// This can only fail if the queue is full, which happens at [SeqNo::MAX] items
    pub fn enqueue(&mut self) -> Result<SeqNo, Error> {
        let index = self.write_index;
        // Normally never happens if capacity is set below SeqNo::MAX
        if self.unconfirmed_count() == SeqNo::MAX {
            Err(Error::QueueFull)
        } else {
            // TODO wrap on largest multiple_of self.capacity instead of u32::MAX
            // otherwise there are weird side effects for the  persistent layer above
            // (discontinuity in which block / fragment is in use)
            // see bug #458
            self.write_index = index.wrapping_add(1);

            if self.unconfirmed_count() > self.capacity {
                // queue is more than full: part of the data in the store
                // must be assumed to be lost (erased) by circular overwrite.

                // todo #458: overflow/wrap behavior
                let erased_until = self
                    .write_index
                    .checked_next_multiple_of(self.overwrite_granularity)
                    .unwrap_or(0)
                    .wrapping_sub(self.capacity);
                self.readers.bump_min_index(erased_until);
            }

            Ok(index)
        }
    }

    /// Get next sequence number for writing
    pub fn writable_seq_no(&self) -> SeqNo {
        self.write_index
    }

    /// Read an iten from the queue
    ///
    /// Each time this method is called, the next item is returned.
    /// This behaves almost like a normal FIFO, but items are not actually dequeued untill
    /// explicitly confirmed via its [ConfirmHandle].
    ///
    /// Up to `MAX_PEEKS` successive elements can be read without confirming.
    /// If more items are peeked, the result from peek_next will keep repeating a sequence
    /// of items that have not been confirmed yet.
    pub fn peek_next(&mut self) -> Result<ConfirmHandle<'ch, MAX_PEEKS>, ReadBlocked> {
        // Nothing to peek
        if self.unconfirmed_count() == 0 {
            return Err(ReadBlocked::Empty);
        }

        self.update_reader_slots(); // TODO async version of this could await completion/cancelation of in-flight fragments
        let seqno = self.try_alloc_reader()?;
        let sender = self.confirm_channel.0.sender().into();
        Ok(ConfirmHandle::new(seqno, sender))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test if the PeekQueue behaves as expected
    #[test]
    fn test_peek_queue_internal_state() {
        // init empty peekqueue of 5 long starting at SeqNo=88
        let mut readers = PeekQueue::<5>::new(88);
        assert_eq!(readers.reader_slots, [ReadState::None; 5]);

        for expected_seq_no in 88..=92 {
            let seq_no = readers.find_reader(99).unwrap();
            assert_eq!(expected_seq_no, seq_no);
        }
        // expect all reader slots busy and placed in slot `seq_no % 5`
        assert_eq!(
            readers.reader_slots,
            [
                ReadState::Busy(90),
                ReadState::Busy(91),
                ReadState::Busy(92),
                ReadState::Busy(88),
                ReadState::Busy(89)
            ]
        );

        // confirm some readers in random order
        readers.update(Confirm {
            id: 89,
            success: false,
        });
        readers.update(Confirm {
            id: 91,
            success: true,
        });
        readers.update(Confirm {
            id: 88,
            success: true,
        });

        // expect all reader slots busy and placed in slot `seq_no % 5`
        assert_eq!(
            readers.reader_slots,
            [
                ReadState::Busy(90),
                ReadState::None,
                ReadState::Busy(92),
                ReadState::None,
                ReadState::Abort(89)
            ]
        );

        // slot 4 is ready for use as no 93
        assert_eq!(93, readers.find_reader(99).unwrap());

        // slot 5 must retry no 89
        assert_eq!(89, readers.find_reader(99).unwrap());

        // slot 0 is next, but still busy (wait in case it becomes a retry, so that we retry 89,90 in sequence)
        match readers.find_reader(99) {
            Err(ReadBlocked::Busy) => {}
            unexpected => {
                panic!("unexpected result: {unexpected:?}");
            }
        }
        assert_eq!(
            readers.reader_slots,
            [
                ReadState::Busy(90),
                ReadState::None,
                ReadState::Busy(92),
                ReadState::Busy(93),
                ReadState::Busy(89)
            ]
        );
    }

    #[test]
    fn enqueue_dequeue_single() {
        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
        q.enqueue().unwrap();
        let h0 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        h0.confirm();
        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }

    #[test]
    fn enqueue_dequeue_multi() {
        simple_logger::init_with_level(log::Level::Debug).ok();
        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
        q.enqueue().unwrap();
        q.enqueue().unwrap();

        let h0 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        h0.confirm();

        let h1 = q.peek_next().unwrap();
        assert_eq!(1, h1.seq_no());
        h1.confirm();

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }

    #[test]
    fn enqueue_dequeue_10() {
        simple_logger::init_with_level(log::Level::Debug).ok();
        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
        for _ in 0..10 {
            q.enqueue().unwrap();
        }

        for i in 0..10 {
            let h = q.peek_next().unwrap();
            assert_eq!(i, h.seq_no());
            h.confirm();
        }

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }

    #[test]
    fn enqueue_batched_dequeue_retry() {
        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
        q.enqueue().unwrap();
        q.enqueue().unwrap();
        let h0 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        // item should be retried
        drop(h0);

        let h1 = q.peek_next().unwrap();
        assert_eq!(1, h1.seq_no());
        h1.confirm();
        let h0 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        h0.confirm();
        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }

    #[test]
    fn enqueue_batched_dequeue_retry_batched() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());

        // Enqueue batched
        q.enqueue().unwrap();
        q.enqueue().unwrap();
        log::debug!("Batch enqueued");

        // Peek + abort in batch
        let h0 = q.peek_next().unwrap();
        let h1 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        assert_eq!(1, h1.seq_no());
        drop(h0);
        drop(h1);
        log::debug!("Batch dropped");

        // Peek + confirm retry in batch
        let h0 = q.peek_next().unwrap();
        let h1 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        assert_eq!(1, h1.seq_no());
        log::debug!("Batch retried");
        h0.confirm();
        h1.confirm();
        log::debug!("Batch confirmed");

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }

    #[test]
    fn enqueue_batched_dequeue_after_multiple_retries() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());

        // Enqueue batched
        q.enqueue().unwrap();
        q.enqueue().unwrap();
        log::debug!("Batch enqueued");

        // Retry should keep working as long as we're dropping (=not confirming)
        for i in 0..10 {
            log::debug!("Retry attempt {i}");
            // Peek + abort in batch
            let h0 = q.peek_next().unwrap();
            let h1 = q.peek_next().unwrap();
            assert_eq!(0, h0.seq_no());
            assert_eq!(1, h1.seq_no());
            drop(h0);
            drop(h1);
            log::debug!("Batch dropped");
        }

        // Finally Peek + confirm
        let h0 = q.peek_next().unwrap();
        let h1 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        assert_eq!(1, h1.seq_no());
        log::debug!("Batch retried");
        h0.confirm();
        h1.confirm();
        log::debug!("Batch confirmed");

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }

    #[test]
    fn enqueue_batched_dequeue_retry_out_of_order() {
        simple_logger::init_with_level(log::Level::Debug).ok();

        let channel = ConfirmChannel::new();
        let mut q = Queue::<3>::new(&channel);

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());

        // Enqueue batched
        q.enqueue().unwrap();
        q.enqueue().unwrap();
        log::debug!("Batch enqueued");

        // Peek + abort in batch
        let h0 = q.peek_next().unwrap();
        let h1 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        assert_eq!(1, h1.seq_no());
        drop(h0);
        drop(h1);
        log::debug!("Batch dropped out-of-order");

        // Peek + confirm retry in batch
        let h0 = q.peek_next().unwrap();
        let h1 = q.peek_next().unwrap();
        assert_eq!(0, h0.seq_no());
        assert_eq!(1, h1.seq_no());
        log::debug!("Batch retried");
        h1.confirm();
        h0.confirm();
        log::debug!("Batch confirmed");

        assert_eq!(ReadBlocked::Empty, q.peek_next().unwrap_err());
    }
}
