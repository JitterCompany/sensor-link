//! The network task: the generic sensor-link network task, with the modem LED
//! following the connection state on the way.

use demo_firmware_core::logic::{
    led::Modem,
    signal::{CommonSignal, DisconnectInfo, NetworkSignal, Signal},
    SendChannel,
};

use crate::{
    app::{self, Connection, NotifyingUploadRx},
    led, MonoTime, SignalTx, NETWORK_ACTION_NOTIFIER,
};
use demo_firmware_core::meta::DeviceDescriptor;

/// Passes signals on to the orchestrator, and sets the modem LED from the
/// connection state they report
struct LedTap<'a>(&'a mut SignalTx);

impl LedTap<'_> {
    fn observe(signal: &Signal) {
        let Signal::Common(CommonSignal::Network(network)) = signal else {
            return;
        };
        match network {
            NetworkSignal::Connected => led::set_modem(Modem::Connected),
            NetworkSignal::ConnectFailed
            | NetworkSignal::Disconnected(DisconnectInfo::Retry(_)) => {
                led::set_modem(Modem::Connecting)
            }
            NetworkSignal::Disconnected(DisconnectInfo::Final) => led::set_modem(Modem::Off),
            _ => {}
        }
    }
}

impl SendChannel<Signal> for LedTap<'_> {
    type Error = <SignalTx as SendChannel<Signal>>::Error;

    async fn send(&mut self, signal: Signal) -> Result<(), Self::Error> {
        Self::observe(&signal);
        self.0.send(signal).await
    }

    fn try_send(&mut self, signal: Signal) -> Result<(), Signal> {
        Self::observe(&signal);
        self.0.try_send(signal)
    }

    fn is_ready(&self) -> bool {
        self.0.is_ready()
    }
}

pub async fn network(
    signal_sender: &mut SignalTx,
    connection: &mut Connection,
    upload_r: &mut NotifyingUploadRx,
    device_descriptor: &mut DeviceDescriptor,
) {
    // The modem powers up as the network task connects
    led::set_modem(Modem::Connecting);
    app::modem_rx::spawn().ok();
    demo_firmware_core::logic::network_task::<_, _, _, _, MonoTime, _, _>(
        &mut LedTap(signal_sender),
        connection,
        &NETWORK_ACTION_NOTIFIER,
        upload_r,
        device_descriptor,
    )
    .await;
    led::set_modem(Modem::Off);
}
