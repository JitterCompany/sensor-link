//! Network logic
//!

use sensor_link_protocol::{
    event::{CellInfo, GnssFix, GnssReport},
    Error, Milliseconds, Topic,
};
use serde::Serialize;

use crate::{
    drivers::time::{self, timestamp_or_default_us},
    logic::{
        client::ClientEvent,
        dispatch::confirmable::Confirmable,
        network::upload::NetworkUploadItem,
        signal::{CmdSource, DisconnectInfo, NetworkSignal, Signal},
        time_adjust, DrainOnDisconnect,
    },
    meta::DeviceMetaDataProvider,
    monotonic_time::{self, delay_ms, traits::MonotonicTime, FutureTimeout, MonotonicInstant},
    mqtt::GnssError,
    serialize::Sendable,
    utils::select::{select3, Select3},
};

use super::{NetworkAction, NetworkActionNotifyReader, NetworkStatus, ReceiveChannel, SendChannel};

pub mod upload;

/// Outcome of handling a single incoming network response (`Op::Read`).
pub enum ReadOutcome {
    /// Response handled; keep the connection open.
    Continue,
    /// The client reported that the connection was dropped.
    Disconnected,
}

/// Operations the network task drives on the protocol client.
///
/// Extracted so [`network_task`] is generic over the concrete client instead of
/// being bound to a single client type. The client-specific dispatch
/// (incoming events, device info/status) lives behind this trait, in
/// `impl NetworkClient for Client<M>` at the bottom of this module.
///
/// Uploads are not a client concern: each upload item owns its send logic via
/// [`NetworkUploadItem`], so the task only needs the generic
/// [`send_sendable`](NetworkClient::send_sendable) primitive here.
/// Result of starting a firmware download.
#[derive(Debug, PartialEq, Eq)]
pub enum FirmwareDownloadStart {
    /// The download started: firmware chunks will follow.
    Started,

    /// The download failed: no firmware chunks will follow.
    Failed,
}

pub trait NetworkClient {
    /// Underlying driver/client error type.
    type ClientError: core::fmt::Debug;

    /// Wire topic this client publishes on (e.g. `sensor_link_protocol::TopicFromDevice`).
    type Topic: Topic;

    /// Orchestrator signal type this client emits. Generic clients emit only the
    /// common subset ([`Signal`]); product clients emit their own superset.
    type Signal: From<Signal>;

    /// Device-status payload this client publishes on the `status` topic.
    ///
    /// Pinned per client (rather than chosen per call) so the status wire format
    /// is fixed by the client, not by whatever the orchestrator hands it.
    type Status: NetworkStatus;

    /// Open the connection (and perform any required subscriptions).
    async fn connect(&mut self, timestamp_ms: Milliseconds)
        -> Result<(), Error<Self::ClientError>>;

    /// Close the connection.
    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>>;

    /// Await a driver response or timeout. Safe to use inside `select`.
    async fn await_response(&mut self, timeout_s: u32) -> Option<()>;

    /// Begin a firmware download.
    ///
    /// `Err` means the connection is broken; see [FirmwareDownloadStart] for the rest.
    async fn download_firmware_update(
        &mut self,
    ) -> Result<FirmwareDownloadStart, Error<Self::ClientError>>;

    /// Send a generic [`Sendable`].
    async fn send_sendable(
        &mut self,
        sendable: &dyn Sendable<Self::Topic>,
    ) -> Result<(), Error<Self::ClientError>>;

    /// Handle one incoming response, dispatching the resulting signals to the
    /// orchestrator (wraps the `Op::Read` arm).
    async fn handle_read<SignalTx: SendChannel<Self::Signal>>(
        &mut self,
        signal_queue: &mut SignalTx,
    ) -> ReadOutcome;

    /// Collect device metadata and publish it
    /// (wraps the `NetworkAction::SendDeviceInfo` arm).
    ///
    /// The provider's device type stays free; the client only serializes it, so
    /// the sole requirement is that it is [`Serialize`](serde::Serialize).
    async fn send_device_info<P>(&mut self, provider: &P) -> Result<(), Error<Self::ClientError>>
    where
        P: DeviceMetaDataProvider,
        P::DeviceType: Serialize;

    /// Publish a device-status payload (wraps the `NetworkAction::SendStatus`
    /// arm). The payload type is pinned by the client via [`Status`](Self::Status):
    /// the client injects the live modem signal strength (which only it can
    /// sample) before publishing.
    async fn send_status(
        &mut self,
        status: &mut Self::Status,
    ) -> Result<(), Error<Self::ClientError>>;

    /// Cell the modem is registered on, if known.
    ///
    /// Implementors may simply return `None` if no cell info can be obtained.
    async fn cell_info(&mut self) -> Option<CellInfo>;

    /// See [MqttClient::gnss_search_s](crate::mqtt::MqttClient::gnss_search_s)
    fn gnss_search_s(&self) -> Option<u16> {
        None
    }

    async fn gnss_start(&mut self) -> Result<(), GnssError> {
        Err(GnssError::Unsupported)
    }

    async fn gnss_fix(&mut self) -> Result<Option<GnssFix>, GnssError> {
        Err(GnssError::Unsupported)
    }

    async fn gnss_stop(&mut self) {}
}

/// Logged once the MQTT session is up. sensor-link-provision looks for this
/// line in the boot log to verify a device, so keep the text stable.
pub const CONNECTED_MARKER: &str = "sensor-link: mqtt connected";

const MIN_RETRY_DELAY_SEC: u32 = 3;

/// This is the absolute worst-case time it could take to connect to server,
/// based on AT command timeout definitions. See issue #459
const NETWORK_CONNECTING_TIMEOUT_SEC: u32 = 11 * 60;

/// Network task gives up reconnects if continuously disconnected for longer than this
const RETRY_TIMEOUT_SEC: u32 = 5 * 60;

/// Network task should never take longer than this to handle NetworkActions
/// This is a combination of the worst-case time to connect to the server + retry timeout
pub const NETWORK_DISCONNECTED_TIMEOUT_MS: u32 =
    (RETRY_TIMEOUT_SEC + NETWORK_CONNECTING_TIMEOUT_SEC) * 1000;

/// Settings the network task timeout to 1 day is effectively
/// infinite as long as there is any activity during the day.
pub const INFINITE_TIMEOUT_S: u32 = 86400;
pub const DEFAULT_TIMEOUT_S: u32 = 20;
const MINIMUM_TIMEOUT_S: u32 = 5;

/// Operation to be performed by network task
enum Op<'u, U, S> {
    Action(NetworkAction<S>),
    Upload(&'u U),
    Read,
    GnssPoll,
    Stop,
    None,
}

/// A GNSS search running during a network session
struct GnssSearch {
    started: MonotonicInstant,
    budget_s: u16,
}

impl GnssSearch {
    async fn start<C: NetworkClient>(client: &mut C) -> Option<Self> {
        let budget_s = client.gnss_search_s()?;
        match client.gnss_start().await {
            Ok(()) => {
                log::info!(target: "Network", "GNSS search started, {budget_s} s");
                Some(Self {
                    started: monotonic_time::now(),
                    budget_s,
                })
            }
            Err(GnssError::Unsupported) => None,
            Err(err) => {
                log::warn!(target: "Network", "GNSS start failed: {err:?}");
                None
            }
        }
    }

    fn report(&self, fix: Option<GnssFix>) -> GnssReport {
        report_after(self.started.elapsed_sec(), fix)
    }

    /// Poll the fix once. Returns the report when the search is over.
    async fn poll<C: NetworkClient>(&self, client: &mut C) -> Option<GnssReport> {
        let result = client.gnss_fix().await;
        search_outcome(result, self.started.elapsed_sec(), self.budget_s)
    }
}

/// The report once the search is over: a fix, an error or the budget used up.
fn search_outcome(
    result: Result<Option<GnssFix>, GnssError>,
    elapsed_s: u64,
    budget_s: u16,
) -> Option<GnssReport> {
    let fix = match result {
        Ok(Some(fix)) => Some(fix),
        Ok(None) if elapsed_s < u64::from(budget_s) => return None,
        Ok(None) => None,
        Err(err) => {
            log::warn!(target: "Network", "GNSS fix failed: {err:?}");
            None
        }
    };
    Some(report_after(elapsed_s, fix))
}

fn report_after(elapsed_s: u64, fix: Option<GnssFix>) -> GnssReport {
    GnssReport {
        search_s: elapsed_s.try_into().unwrap_or(u16::MAX),
        fix,
    }
}

impl<'u, U, S> Op<'u, U, S> {
    fn is_upload(&self) -> bool {
        matches!(self, Op::Upload(_))
    }
}

/// Reason for disconnecting
#[derive(Debug)]
enum DisconnectReason {
    /// Disconnection was intended
    Success,

    /// Connection failed (was never connected)
    NotConnected,

    /// Unexpected disconnect
    Unexpected,

    /// Error while connected
    Error,

    /// Unexpected error, don't bother retry without modem reset
    Fatal,
}

struct State<U> {
    // Initiall network timeout setting
    timeout_s: u32,

    // Message pending for upload
    pending_upload: Option<Confirmable<U>>,
}

pub async fn network_task<
    SignalTx: SendChannel<C::Signal>,
    ActionList: NetworkActionNotifyReader<C::Status>,
    MsgRx: ReceiveChannel<Confirmable<U>> + DrainOnDisconnect<U>,
    C: NetworkClient,
    T: MonotonicTime,
    U: NetworkUploadItem<C>,
    P: DeviceMetaDataProvider,
>(
    signal_queue: &mut SignalTx,
    client: &mut C,
    action_list: ActionList,
    msg_qeue: &mut MsgRx,
    provider: &mut P,
) where
    P::DeviceType: Serialize,
{
    // TODO fix bug #481: this state gets lost after disconnect.
    // The pending message is lost and the timeout changes back to default
    let mut state = State {
        timeout_s: DEFAULT_TIMEOUT_S,
        pending_upload: None,
    };
    let mut offline_since = Some(monotonic_time::now());
    let mut next_retry_sec = MIN_RETRY_DELAY_SEC;

    // Retry loop
    loop {
        let timestamp = Milliseconds::from_raw_microseconds(timestamp_or_default_us());
        let disconnect_reason = match client
            .connect(timestamp)
            .with_timeout_ms(NETWORK_CONNECTING_TIMEOUT_SEC * 1000)
            .await
        {
            Some(Err(err)) => {
                log::warn!("Network Connect Error: {err:?}");
                // Disconnect to reset internal state
                client.disconnect().await.ok();
                DisconnectReason::NotConnected
            }
            None => {
                // Timeout for the whole connect command is never expected.
                // This would be a bug as either
                // 1. NETWORK_CONNECTING_TIMEOUT_SEC is defined wrong (should be the sum of all sub steps)
                // 2. modem hangs somehow
                // in both conditions a retry without modem reset is unlikely
                log::warn!("Network Connect Timeout");
                // Disconnect to reset internal state
                client.disconnect().await.ok();
                DisconnectReason::Fatal
            }
            Some(Ok(_)) => {
                log::info!("{CONNECTED_MARKER}");
                offline_since = None;

                signal_queue
                    .send(Signal::Network(NetworkSignal::Connected).into())
                    .await
                    .ok();

                if let Some(cell) = client.cell_info().await {
                    signal_queue.send(Signal::CellInfo(cell).into()).await.ok();
                }

                // Handle network traffic as long as the connection is open
                let disconnect_reason = handle_connection::<SignalTx, ActionList, MsgRx, C, U, P>(
                    &mut state,
                    signal_queue,
                    client,
                    &action_list,
                    msg_qeue,
                    provider,
                )
                .await;

                // Make sure client is disconnected
                match client.disconnect().await {
                    Ok(_) => log::info!(target: "Network", "Disconnected"),
                    Err(_) => log::error!(target: "Network", "Failed to disconnect"),
                };

                disconnect_reason
            }
        };

        let mut will_retry = match offline_since {
            None => {
                offline_since = Some(monotonic_time::now());
                true
            }
            // Retry only if the next connect can be made within RETRY_TIMEOUT_SEC since last disconnect.
            // Assuming connecting never takes longer than `NETWORK_CONNECTING_TIMEOUT_SEC`
            // this should guarantee the total non-connected-time stays below NETWORK_DISCONNECTED_TIMEOUT_MS
            Some(ref time) => (time.elapsed_sec() as u32 + next_retry_sec) <= RETRY_TIMEOUT_SEC,
        };

        // Send signal with the disconnect status
        let sig = match disconnect_reason {
            // Send disconnect signal and exit
            DisconnectReason::Success => {
                log::info!("Succesfully disconnected: done!");
                signal_queue
                    .send(
                        Signal::Network(NetworkSignal::Disconnected(DisconnectInfo::Final)).into(),
                    )
                    .await
                    .ok();
                return;
            }

            DisconnectReason::NotConnected => NetworkSignal::ConnectFailed,
            DisconnectReason::Fatal => {
                will_retry = false;
                NetworkSignal::Disconnected(DisconnectInfo::Final)
            }
            DisconnectReason::Error | DisconnectReason::Unexpected => match will_retry {
                false => NetworkSignal::Disconnected(DisconnectInfo::Final),
                true => NetworkSignal::Disconnected(DisconnectInfo::Retry(next_retry_sec)),
            },
        };
        signal_queue.send(Signal::Network(sig).into()).await.ok();

        if !will_retry {
            log::error!(
                "Unable to connect: give up after {:?} (retry would be {next_retry_sec})",
                offline_since.map(|t| t.elapsed_sec())
            );
            return;
        }

        log::info!("Retrying after {next_retry_sec} sec...");
        delay_ms(next_retry_sec * 1000).await;

        // exponential backoff for next retry
        next_retry_sec *= 2;
        if next_retry_sec > RETRY_TIMEOUT_SEC {
            next_retry_sec = RETRY_TIMEOUT_SEC;
        }
    }
}

/// Handle the connection, assuming it is already open
async fn handle_connection<
    SignalTx: SendChannel<C::Signal>,
    ActionList: NetworkActionNotifyReader<C::Status>,
    MsgRx: ReceiveChannel<Confirmable<U>> + DrainOnDisconnect<U>,
    C: NetworkClient,
    U: NetworkUploadItem<C>,
    P: DeviceMetaDataProvider,
>(
    state: &mut State<U>,
    signal_queue: &mut SignalTx,
    client: &mut C,
    action_list: &ActionList,
    msg_qeue: &mut MsgRx,
    provider: &P,
) -> DisconnectReason
where
    P::DeviceType: Serialize,
{
    // Disconnect is treated as a hint: once set, drain action_list + background
    // channels (NOT upload_r) and exit only when everything in-flight is gone.
    let mut disconnect_pending = false;

    // While a GNSS search runs, wait at most 1 s for the modem, so a timeout
    // polls the fix instead of ending the session.
    let mut gnss = GnssSearch::start(client).await;

    let reason = loop {
        let mut op = {
            // Upload still pending for retry
            if let Some(upload) = &state.pending_upload {
                if let Some(action) = action_list.try_next_action() {
                    Op::Action(action)
                } else {
                    Op::Upload(upload)
                }

            // Disconnect hint received: drain remaining in-flight work without
            // consuming upload_r, then exit when nothing is left.
            } else if disconnect_pending {
                if let Some(action) = action_list.try_next_action() {
                    Op::Action(action)
                } else if let Some(msg) = msg_qeue.try_recv_drain_only() {
                    Op::Upload(state.pending_upload.insert(msg))
                } else if gnss.is_some() {
                    match client.await_response(1).await {
                        Some(_) => Op::Read,
                        None => Op::GnssPoll,
                    }
                } else {
                    break DisconnectReason::Success;
                }

            // Await modem activity or incoming commands, data
            } else {
                // Futures for commands / requests from other tasks
                let cmd_fut = action_list.next_action();
                let msg_fut = msg_qeue.recv();

                // Future for incoming events from the network client
                let timeout_s = if gnss.is_some() { 1 } else { state.timeout_s };
                let poll_fut = client.await_response(timeout_s);

                // Wait for an event on either future
                match select3(poll_fut, cmd_fut, msg_fut).await {
                    // Incomming messages from the network are already parsed by the Client
                    Select3::A(recv) => match recv {
                        Some(_) => Op::Read,
                        None if gnss.is_some() => Op::GnssPoll,
                        None => Op::Stop, // Timeout or uart error.
                    },
                    Select3::B(action) => Op::Action(action),
                    Select3::C(msg) => match msg {
                        Ok(msg) => Op::Upload(state.pending_upload.insert(msg)),
                        Err(err) => {
                            log::error!("Failed to read upload queue: {err:?}");
                            Op::None
                        }
                    },
                }
            }
        };

        // Computed before the match because `match op` consumes `op` (its
        // borrow of `state.pending_upload` ends), and the `Ok` arm below needs
        // to `take()` the pending upload to confirm it.
        let is_upload = op.is_upload();

        let mut op_result: Result<(), Error<C::ClientError>> = Ok(());
        match op {
            Op::Read => match client.handle_read(signal_queue).await {
                ReadOutcome::Continue => {}
                ReadOutcome::Disconnected => break DisconnectReason::Unexpected,
            },
            Op::Action(NetworkAction::Disconnect) => {
                log::info!(target: "Network", "Disconnect hint received; draining");
                disconnect_pending = true;
            }
            Op::Action(NetworkAction::SetTimeout(new_timeout)) => {
                state.timeout_s = new_timeout.max(MINIMUM_TIMEOUT_S);
            }
            Op::Action(NetworkAction::SendDeviceInfo) => {
                op_result = client.send_device_info(provider).await;
            }
            Op::Action(NetworkAction::SendStatus(ref mut device_status)) => {
                op_result = client.send_status(device_status).await;
                if op_result.is_ok() {
                    let token = device_status.confirmation_token();
                    signal_queue
                        .send(Signal::StatusSent(token).into())
                        .await
                        .ok();
                }
            }
            Op::Action(NetworkAction::DownloadUpdate) => {
                match client.download_firmware_update().await {
                    Ok(FirmwareDownloadStart::Started) => {}
                    // No chunks will follow, so no other signal will report this.
                    Ok(FirmwareDownloadStart::Failed) => {
                        signal_queue
                            .send(Signal::FirmwareUpdateFailed.into())
                            .await
                            .ok();
                    }
                    Err(err) => {
                        log::error!("Network: Failed to download update: {err:?}");
                        break DisconnectReason::Error;
                    }
                }
            }
            Op::Upload(upload) => {
                op_result = upload.send(client).await;
                if op_result.is_err() {
                    log::error!("Network: Failed to send upload");
                }
            }
            Op::GnssPoll => {
                if let Some(search) = &gnss {
                    if let Some(report) = search.poll(client).await {
                        finish_gnss(client, signal_queue, report).await;
                        gnss = None;
                    }
                }
            }
            Op::None => {}
            Op::Stop => break DisconnectReason::Success,
        };
        match op_result {
            Err(Error::InvalidSIM)
            | Err(Error::Client(_))
            | Err(Error::TimeOut)
            | Err(Error::MQTT(_)) => break DisconnectReason::Error,

            Err(Error::Serialize) => {
                // don't disconnect: give other topics a chance to still work.
                // otherwise a bug in one topic could 'brick' the network connection
            }
            Err(Error::SerializeTopic(_topic_err)) => {
                // don't disconnect: give other topics a chance to still work.
                // otherwise a bug in one topic could 'brick' the network connection
            }
            Ok(_) => {
                // if an upload has completed, confirm it.
                // Note: so far we only support one in-flight upload.
                // To support multiple in-flight uploads, we need to:
                // 1. make pending_upload a list/map
                // 2. confirm the correct upload (probably based on MQTT message id)
                if is_upload {
                    if let Some(completed) = state.pending_upload.take() {
                        completed.confirm();
                    }
                }
            }
        }
    };

    // Session ended before the search did: report it as not found
    if let Some(search) = gnss {
        finish_gnss(client, signal_queue, search.report(None)).await;
    }
    reason
}

async fn finish_gnss<C: NetworkClient, SignalTx: SendChannel<C::Signal>>(
    client: &mut C,
    signal_queue: &mut SignalTx,
    report: GnssReport,
) {
    client.gnss_stop().await;
    log::info!(target: "Network", "GNSS search done: {report:?}");
    signal_queue.send(Signal::Gnss(report).into()).await.ok();
}

/// Converts a manufacturer-generic client event into the orchestrator signal it
/// carries.
///
/// Shared by every [`NetworkClient`] implementation: [`ClientEvent`] is the
/// common `jitter-sensor-link` event type, so any client (the generic one, or a
/// product wrapper handling its own `Common` arm) can funnel through here.
///
/// Events that carry no signal map to the [`ReadOutcome`] the caller should
/// return instead: most keep the connection open ([`ReadOutcome::Continue`]),
/// while [`ClientEvent::Disconnected`] reports the drop
/// ([`ReadOutcome::Disconnected`]).
impl TryFrom<ClientEvent> for Signal {
    type Error = ReadOutcome;

    fn try_from(event: ClientEvent) -> Result<Self, Self::Error> {
        Ok(match event {
            ClientEvent::ServerStatus(_status) => {
                // TODO: do something useful with this information
                return Err(ReadOutcome::Continue);
            }
            ClientEvent::CommandReceived(cmd) => Signal::Command(cmd, CmdSource::Network),
            ClientEvent::TimestampReceived(t_network) => {
                let t_local = time::timestamp_or_default_us();
                Signal::NetworkTime(
                    time_adjust::Timestamp::OffsetAdjusted(t_local),
                    time_adjust::NetworkTime {
                        timestamp_server_us: t_network,
                        latency_estimate_us: 2_000_000, // assuming <= 2 seconds. TODO #427: better estimate? maybe based on dt from published status?
                    },
                )
            }
            ClientEvent::FWChunkReceived(chunk) => Signal::FirmwareChunk(chunk),
            ClientEvent::FirmwareUpdateAnnounced => Signal::FirmwareUpdateAnnounced,
            ClientEvent::FWDownloadComplete => Signal::FirmwareUpdateComplete,
            ClientEvent::FWDownloadFailed => Signal::FirmwareUpdateFailed,
            ClientEvent::Disconnected => return Err(ReadOutcome::Disconnected),
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    fn fix() -> GnssFix {
        GnssFix {
            lat: 52.0,
            lon: 5.0,
            alt: 1.0,
            hdop: 0.9,
            sat: 8,
        }
    }

    #[test]
    fn gnss_fix_ends_search() {
        let report = search_outcome(Ok(Some(fix())), 31, 120).unwrap();
        assert_eq!(report.search_s, 31);
        assert_eq!(report.fix, Some(fix()));
    }

    #[test]
    fn gnss_keeps_searching_within_budget() {
        assert_eq!(search_outcome(Ok(None), 119, 120), None);
    }

    #[test]
    fn gnss_gives_up_after_budget() {
        let report = search_outcome(Ok(None), 120, 120).unwrap();
        assert_eq!(report.search_s, 120);
        assert_eq!(report.fix, None);
    }

    #[test]
    fn gnss_modem_error_ends_search() {
        let report = search_outcome(Err(GnssError::Modem), 3, 120).unwrap();
        assert_eq!(report.fix, None);
    }
}
