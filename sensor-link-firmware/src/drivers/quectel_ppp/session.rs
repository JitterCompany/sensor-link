//! Reusable session buffers.
//!
//! One MQTT-over-TLS session exists at a time, but the network task
//! reconnects in place, so the socket/TLS/MQTT buffers must be reclaimable.
//! `StaticCell` cannot re-initialize; this pool hands out `&'static mut`
//! references from plain statics under a taken-flag.
//!
//! Safety invariant: `take()` panics while a lease is outstanding, and
//! `release()` may only be called after every reference derived from the
//! lease has been dropped (the driver drops its `Session` first). All
//! accesses happen from the single network task, the flag is atomic anyway.

use core::sync::atomic::{AtomicBool, Ordering};

use super::mqtt_core::{ReceiveBuffer, RECEIVE_BUF_LEN};
use super::tls::{TLS_RX_BUF_LEN, TLS_TX_BUF_LEN};

pub(crate) const TCP_RX_LEN: usize = 4096;
pub(crate) const TCP_TX_LEN: usize = 2048;

static TAKEN: AtomicBool = AtomicBool::new(false);

static mut TCP_RX: [u8; TCP_RX_LEN] = [0; TCP_RX_LEN];
static mut TCP_TX: [u8; TCP_TX_LEN] = [0; TCP_TX_LEN];
static mut TLS_RX: [u8; TLS_RX_BUF_LEN] = [0; TLS_RX_BUF_LEN];
static mut TLS_TX: [u8; TLS_TX_BUF_LEN] = [0; TLS_TX_BUF_LEN];
static mut MQTT_RECV: [u8; RECEIVE_BUF_LEN] = [0; RECEIVE_BUF_LEN];
/// Slot for the per-session `ReceiveBuffer` value, so the MQTT core can
/// borrow it with a `'static` lifetime.
static mut MQTT_BUMP: Option<ReceiveBuffer<'static>> = None;

pub(crate) struct SessionLease {
    pub tcp_rx: &'static mut [u8; TCP_RX_LEN],
    pub tcp_tx: &'static mut [u8; TCP_TX_LEN],
    pub tls_rx: &'static mut [u8; TLS_RX_BUF_LEN],
    pub tls_tx: &'static mut [u8; TLS_TX_BUF_LEN],
    pub mqtt_bump: &'static mut ReceiveBuffer<'static>,
}

/// Hands out the session buffers. Panics if the previous lease was not
/// [`release`]d.
pub(crate) fn take() -> SessionLease {
    assert!(
        !TAKEN.swap(true, Ordering::AcqRel),
        "session buffers taken twice"
    );
    // Safety: the taken flag guarantees no other reference exists; see the
    // module invariant for the release side.
    unsafe {
        let bump_slot = &mut *core::ptr::addr_of_mut!(MQTT_BUMP);
        *bump_slot = Some(ReceiveBuffer::new(&mut *core::ptr::addr_of_mut!(MQTT_RECV)));
        SessionLease {
            tcp_rx: &mut *core::ptr::addr_of_mut!(TCP_RX),
            tcp_tx: &mut *core::ptr::addr_of_mut!(TCP_TX),
            tls_rx: &mut *core::ptr::addr_of_mut!(TLS_RX),
            tls_tx: &mut *core::ptr::addr_of_mut!(TLS_TX),
            mqtt_bump: bump_slot.as_mut().unwrap(),
        }
    }
}

/// Re-borrows the bump-buffer slot of the current lease (the lease's own
/// reference may have been consumed separately from the other buffers).
///
/// # Safety
///
/// A lease must be taken, and no other reference to the slot may be live.
pub(crate) unsafe fn bump() -> &'static mut ReceiveBuffer<'static> {
    debug_assert!(TAKEN.load(Ordering::Acquire));
    unsafe {
        (*core::ptr::addr_of_mut!(MQTT_BUMP))
            .as_mut()
            .expect("bump slot empty: no lease taken")
    }
}

/// Returns the buffers to the pool.
///
/// # Safety
///
/// Every reference handed out by the matching [`take`] (including everything
/// built on top: socket, TLS connection, MQTT client) must have been dropped.
pub(crate) unsafe fn release() {
    TAKEN.store(false, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One combined test: the pool is a process-wide singleton.
    #[test]
    fn lease_lifecycle() {
        let lease = take();
        assert_eq!(lease.tcp_rx.len(), TCP_RX_LEN);

        // Double take must panic while the lease is out.
        let result = std::panic::catch_unwind(|| take());
        assert!(result.is_err(), "double take must panic");

        drop(lease);
        unsafe { release() };

        // Reusable after release.
        let lease = take();
        drop(lease);
        unsafe { release() };
    }
}
