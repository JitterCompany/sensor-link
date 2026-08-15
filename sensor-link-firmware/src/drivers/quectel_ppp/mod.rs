//! PPP-based Quectel modem driver.
//!
//! Drop-in alternative to [`super::quectel`] behind the [`crate::mqtt::MqttClient`]
//! trait. The modem is used only as a bit-pipe: AT bring-up, then PPP data mode
//! (`ATD*99#`) with the full network stack on the MCU:
//! embassy-net (TCP/IP over PPP) → embedded-tls (mutual TLS, keys never leave
//! the MCU) → rust-mqtt (MQTT 5, QoS 1).
//!
//! This module is the only place where embedded-io-async 0.7 / heapless 0.9
//! types are used; conversions to the crate-wide 0.6 / 0.8 types happen at the
//! `MqttClient` boundary.

pub(crate) mod at_bringup;
pub mod mqtt_core;
pub mod pem;
mod session;
pub mod tls;
pub mod uart_adapter;

use core::cell::RefCell;
use core::net::Ipv4Addr;
use core::str::FromStr;

use embassy_net::tcp::TcpSocket;
use embassy_net::{ConfigV4, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::channel::Channel;
use embedded_io_async::{Read, Write};
use embedded_tls::{TlsConfig, TlsConnection, TlsContext};
use heapless::String;
use embedded_tls::CryptoRngCore;
use sensor_link_protocol::info::VersionString;
use sensor_link_protocol::{Error, MAX_FILE_CHUNK_LEN, MAX_TOPIC_LEN};
use static_cell::StaticCell;

use crate::drivers::quectel::variant::ModemVariant;
use crate::monotonic_time::FutureTimeout;
use crate::mqtt::{Event, FileError, MqttClient, Will};
use crate::traits::{BaudRateControl, ErrorReport, Split, Suspend, Suspendable};
use crate::utils::select::{select2, Select2};

use at_bringup::{AtEngine, BringupConfig, ModemInfo};
use mqtt_core::MqttCore;
use tls::{BtbCryptoProvider, CipherSuite, TlsMaterial};
use uart_adapter::{PppIo, FILL_BUF_LEN};

pub use crate::drivers::quectel::{Config, Credentials};

/// PDP context used in `AT+CGDCONT`; PPP negotiation activates it.
const PPP_CONTEXT_ID: u8 = 1;
/// Waiting for the PPP link + IPCP after requesting the link up.
const LINK_UP_TIMEOUT_MS: u32 = 180_000;
/// DNS / TCP connect / TLS handshake step budget.
const STEP_TIMEOUT_MS: u32 = 30_000;
/// Backoff between bring-up attempts while the link is wanted.
const BRINGUP_RETRY_MS: u32 = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverError {
    /// PPP link did not come up in time.
    LinkTimeout,
    /// DNS resolution failed.
    Dns,
    /// TCP connect failed.
    Tcp,
    /// TLS handshake failed.
    Tls,
    /// MQTT-level failure.
    Mqtt,
}

#[derive(Debug)]
pub enum InitError {
    /// CA / client cert / client key rejected.
    Credentials(tls::MaterialError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkCommand {
    Up,
    Down,
}

type CmdChannel = Channel<CriticalSectionRawMutex, LinkCommand, 4>;
static CMD: CmdChannel = Channel::new();
static MODEM_INFO: BlockingMutex<CriticalSectionRawMutex, RefCell<ModemInfo>> =
    BlockingMutex::new(RefCell::new(ModemInfo {
        signal_quality: None,
        model: String::new(),
        fw_version: String::new(),
    }));

static PPP_STATE: StaticCell<embassy_net_ppp::State<2, 2>> = StaticCell::new();
static STACK_RESOURCES: StaticCell<StackResources<3>> = StaticCell::new();
static PPP_FILL_BUF: StaticCell<[u8; FILL_BUF_LEN]> = StaticCell::new();

/// Creates the driver pair: the frontend implements [`MqttClient`], the
/// backend owns the UART and runs the link (spawn [`QuectelPppBackend::run`]
/// as a task, like the AT driver's `read_incoming`).
///
/// Only a single instance per firmware image is supported (static buffers).
pub fn new<W, R, V, U, RNG>(
    uart: impl Split<W, R, U>,
    power: V,
    config: Config,
    credentials: Credentials,
    mut rng: RNG,
) -> Result<(QuectelPpp<RNG>, QuectelPppBackend<W, R, V, U>), InitError>
where
    W: Write,
    R: Read + ErrorReport + Suspendable,
    V: ModemVariant,
    U: Suspend + BaudRateControl,
    RNG: CryptoRngCore,
{
    let material = TlsMaterial::from_pem(
        credentials.ca_cert.as_bytes(),
        credentials.client_cert,
        credentials.client_key,
    )
    .map_err(InitError::Credentials)?;

    let (device, ppp_runner) = embassy_net_ppp::new(PPP_STATE.init(embassy_net_ppp::State::new()));
    let seed = u64::from(rng.next_u32()) << 32 | u64::from(rng.next_u32());
    let (stack, net_runner) = embassy_net::new(
        device,
        embassy_net::Config::default(),
        STACK_RESOURCES.init(StackResources::new()),
        seed,
    );

    let (uart, tx, rx) = uart.split();

    Ok((
        QuectelPpp {
            stack,
            config,
            material,
            rng,
            session: None,
        },
        QuectelPppBackend {
            tx,
            rx,
            uart,
            power,
            engine: AtEngine::take(),
            ppp_runner,
            net_runner,
            stack,
            fill_buf: PPP_FILL_BUF.init([0; FILL_BUF_LEN]),
        },
    ))
}

// ---------------------------------------------------------------------------
// Backend: owns the UART, drives AT bring-up and the PPP + network runners.
// ---------------------------------------------------------------------------

pub struct QuectelPppBackend<W, R, V, U> {
    tx: W,
    rx: R,
    uart: U,
    power: V,
    engine: AtEngine,
    ppp_runner: embassy_net_ppp::Runner<'static>,
    net_runner: embassy_net::Runner<'static, embassy_net_ppp::Device<'static>>,
    stack: Stack<'static>,
    fill_buf: &'static mut [u8; FILL_BUF_LEN],
}

impl<W, R, V, U> QuectelPppBackend<W, R, V, U>
where
    W: Write,
    R: Read + ErrorReport + Suspendable,
    V: ModemVariant,
    U: Suspend + BaudRateControl,
{
    /// Drives the modem link; never returns. Spawn as its own task.
    pub async fn run(&mut self) -> ! {
        let Self {
            tx,
            rx,
            uart,
            power,
            engine,
            ppp_runner,
            net_runner,
            stack,
            fill_buf,
        } = self;

        let control = async {
            let mut desired = LinkCommand::Down;
            loop {
                // Latest queued command wins.
                while let Ok(cmd) = CMD.try_receive() {
                    desired = cmd;
                }
                if desired == LinkCommand::Down {
                    desired = CMD.receive().await;
                    continue;
                }

                let bringup_cfg = BringupConfig {
                    // Flow-control wiring is a board/app decision; the pins
                    // are configured on the UART before the driver sees it.
                    flow_control: false,
                    context_id: PPP_CONTEXT_ID,
                };
                let info = match engine.bringup(tx, rx, uart, power, &bringup_cfg).await {
                    Ok(info) => info,
                    Err(e) => {
                        log::warn!(target: "quectel-ppp", "bring-up failed: {e:?}");
                        if let Some(cmd) = CMD.receive().with_timeout_ms(BRINGUP_RETRY_MS).await {
                            desired = cmd;
                        }
                        continue;
                    }
                };
                MODEM_INFO.lock(|slot| *slot.borrow_mut() = info);

                // PPP data mode until the link dies or a Down arrives.
                log::info!(target: "quectel-ppp", "entering PPP data mode");
                let ppp = {
                    let io = PppIo::new(&mut *rx, &mut *tx, &mut fill_buf[..]);
                    let ppp_config = embassy_net_ppp::Config {
                        username: b"",
                        password: b"",
                    };
                    let stack = *stack;
                    ppp_runner.run(io, ppp_config, move |status| {
                        stack.set_config_v4(ipv4_config(&status));
                    })
                };
                match select2(ppp, CMD.receive()).await {
                    Select2::A(result) => {
                        log::warn!(target: "quectel-ppp", "PPP ended: {result:?}");
                        stack.set_config_v4(ConfigV4::None);
                        // Modem fell back to command mode (or died); the next
                        // loop iteration re-runs the bring-up.
                        if let Some(cmd) = CMD.receive().with_timeout_ms(BRINGUP_RETRY_MS).await {
                            desired = cmd;
                        }
                    }
                    Select2::B(cmd) => {
                        desired = cmd;
                        if desired == LinkCommand::Down {
                            stack.set_config_v4(ConfigV4::None);
                            engine.shutdown(tx, rx).await;
                            uart.suspend();
                            power.power_off();
                            log::info!(target: "quectel-ppp", "modem powered down");
                        }
                    }
                }
            }
        };

        match select2(control, net_runner.run()).await {
            Select2::A(never) => never,
            Select2::B(never) => never,
        }
    }
}

fn ipv4_config(status: &embassy_net_ppp::Ipv4Status) -> ConfigV4 {
    let Some(address) = status.address else {
        return ConfigV4::None;
    };
    // embassy-net uses heapless 0.9 types.
    let mut dns_servers = heapless09::Vec::new();
    for server in status.dns_servers.iter().flatten() {
        let _ = dns_servers.push(*server);
    }
    ConfigV4::Static(StaticConfigV4 {
        // Point-to-point link: no subnet, no gateway.
        address: Ipv4Cidr::new(address, 32),
        gateway: None,
        dns_servers,
    })
}

// ---------------------------------------------------------------------------
// Frontend: MqttClient implementation over the stack handle.
// ---------------------------------------------------------------------------

/// Either a TLS session or a plain TCP socket (`config.use_tls`).
enum Transport {
    Tls(TlsConnection<'static, TcpSocket<'static>, CipherSuite>),
    Plain(TcpSocket<'static>),
}

impl embedded_io_async_07::ErrorType for Transport {
    type Error = embedded_io_async_07::ErrorKind;
}

impl embedded_io_async_07::Read for Transport {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        use embedded_io_async_07::Error;
        match self {
            Transport::Tls(tls) => tls.read(buf).await.map_err(|e| e.kind()),
            Transport::Plain(socket) => socket.read(buf).await.map_err(|e| e.kind()),
        }
    }
}

impl embedded_io_async_07::Write for Transport {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        use embedded_io_async_07::Error;
        match self {
            Transport::Tls(tls) => tls.write(buf).await.map_err(|e| e.kind()),
            Transport::Plain(socket) => socket.write(buf).await.map_err(|e| e.kind()),
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        use embedded_io_async_07::Error;
        match self {
            Transport::Tls(tls) => tls.flush().await.map_err(|e| e.kind()),
            Transport::Plain(socket) => socket.flush().await.map_err(|e| e.kind()),
        }
    }
}

pub struct QuectelPpp<RNG> {
    stack: Stack<'static>,
    config: Config,
    material: TlsMaterial,
    rng: RNG,
    session: Option<MqttCore<'static, Transport>>,
}

impl<RNG: CryptoRngCore> QuectelPpp<RNG> {
    /// Tears the session down and returns the buffers to the pool.
    fn drop_session(&mut self) {
        if self.session.take().is_some() {
            // Safety: the session (socket, TLS, MQTT core — every user of the
            // lease) was just dropped.
            unsafe { session::release() };
        }
    }

    /// Opens TCP (+ TLS) using the buffers of an already-taken lease.
    async fn open_transport(
        &mut self,
        lease: session::SessionLease,
    ) -> Result<Transport, Error<DriverError>> {
        let address = match Ipv4Addr::from_str(self.config.host) {
            Ok(ip) => embassy_net::IpAddress::Ipv4(ip),
            Err(_) => {
                let addresses = self
                    .stack
                    .dns_query(self.config.host, embassy_net::dns::DnsQueryType::A)
                    .with_timeout_ms(STEP_TIMEOUT_MS)
                    .await
                    .ok_or(Error::TimeOut)?
                    .map_err(|_| Error::Client(DriverError::Dns))?;
                *addresses.first().ok_or(Error::Client(DriverError::Dns))?
            }
        };

        let mut socket = TcpSocket::new(self.stack, lease.tcp_rx, lease.tcp_tx);
        socket
            .connect((address, self.config.port))
            .with_timeout_ms(STEP_TIMEOUT_MS)
            .await
            .ok_or(Error::TimeOut)?
            .map_err(|_| Error::Client(DriverError::Tcp))?;

        if !self.config.use_tls {
            return Ok(Transport::Plain(socket));
        }

        let mut tls = TlsConnection::new(socket, lease.tls_rx, lease.tls_tx);
        let tls_config = TlsConfig::new().with_server_name(self.config.host);
        let provider = BtbCryptoProvider::new(&self.material, &mut self.rng);
        tls.open(TlsContext::new(&tls_config, provider))
            .with_timeout_ms(STEP_TIMEOUT_MS)
            .await
            .ok_or(Error::TimeOut)?
            .map_err(|e| {
                log::error!(target: "quectel-ppp", "TLS handshake failed: {e:?}");
                Error::Client(DriverError::Tls)
            })?;
        Ok(Transport::Tls(tls))
    }

    async fn connect_inner(
        &mut self,
        client_id: &str,
        will: Will,
        clean_session: bool,
    ) -> Result<(), Error<DriverError>> {
        self.drop_session();

        CMD.send(LinkCommand::Up).await;
        self.stack
            .wait_config_up()
            .with_timeout_ms(LINK_UP_TIMEOUT_MS)
            .await
            .ok_or(Error::Client(DriverError::LinkTimeout))?;

        let lease = session::take();
        let bump = unsafe { session::bump() };
        let transport = match self.open_transport(lease).await {
            Ok(t) => t,
            Err(e) => {
                // Everything built on the lease is gone at this point.
                unsafe { session::release() };
                return Err(e);
            }
        };

        let mut core = MqttCore::new(bump);
        match core
            .connect(transport, client_id, &will, !clean_session)
            .await
        {
            Ok(()) => {
                self.session = Some(core);
                Ok(())
            }
            Err(e) => {
                drop(core);
                unsafe { session::release() };
                log::error!(target: "quectel-ppp", "MQTT connect failed: {e:?}");
                Err(Error::Client(DriverError::Mqtt))
            }
        }
    }

    fn core(&mut self) -> Result<&mut MqttCore<'static, Transport>, Error<DriverError>> {
        self.session.as_mut().ok_or(Error::MQTT("not connected"))
    }
}

impl<RNG: CryptoRngCore> MqttClient for QuectelPpp<RNG> {
    type ClientError = DriverError;
    type PollError = ();

    async fn connect(&mut self, client_id: &str, will: Will) -> Result<(), Error<DriverError>> {
        self.connect_inner(client_id, will, true).await
    }

    async fn reconnect(&mut self) -> Result<(), Error<DriverError>> {
        // Dead code in practice: the network task always runs full connects.
        Err(Error::MQTT("reconnect unsupported; use connect"))
    }

    async fn disconnect(&mut self) -> Result<(), Error<DriverError>> {
        if let Some(mut core) = self.session.take() {
            core.disconnect().await;
            drop(core);
            unsafe { session::release() };
        }
        CMD.send(LinkCommand::Down).await;
        Ok(())
    }

    async fn subscribe(&mut self, topic: String<MAX_TOPIC_LEN>) -> Result<(), Error<DriverError>> {
        self.core()?
            .subscribe(topic.as_str())
            .await
            .map_err(|_| Error::Client(DriverError::Mqtt))
    }

    async fn unsubscribe(
        &mut self,
        topic: String<MAX_TOPIC_LEN>,
    ) -> Result<(), Error<DriverError>> {
        self.core()?
            .unsubscribe(topic.as_str())
            .await
            .map_err(|_| Error::Client(DriverError::Mqtt))
    }

    async fn publish(
        &mut self,
        topic: String<MAX_TOPIC_LEN>,
        message: &[u8],
    ) -> Result<(), Error<DriverError>> {
        self.core()?
            .publish(topic.as_str(), message)
            .await
            .map_err(|_| Error::Client(DriverError::Mqtt))
    }

    async fn await_response(&mut self, timeout_s: u32) -> Result<(), Self::PollError> {
        match self.session.as_mut() {
            Some(core) => core.await_event(timeout_s).await.map(|_| ()),
            None => Err(()),
        }
    }

    async fn handle_response(&mut self) -> Result<Event, ()> {
        let core = self.session.as_mut().ok_or(())?;
        if let Some(message) = core.take_message() {
            return Ok(Event::ReceivedMessage(message));
        }
        if core.take_lost() {
            return Ok(Event::Disconnected);
        }
        Err(())
    }

    async fn download_file(&mut self, _url: &str) -> Result<(), Error<DriverError>> {
        // Streaming OTA over the own stack lands in a follow-up work package.
        Err(Error::MQTT("OTA download not implemented on PPP driver"))
    }

    async fn read_file_chunk(
        &mut self,
        _chunk_size: usize,
    ) -> Result<heapless::Vec<u8, MAX_FILE_CHUNK_LEN>, FileError> {
        Err(FileError::FileNotFound)
    }

    async fn signal_quality(&mut self) -> Option<i16> {
        MODEM_INFO.lock(|slot| slot.borrow().signal_quality)
    }

    async fn modem_model(&mut self) -> VersionString {
        MODEM_INFO.lock(|slot| slot.borrow().model.clone())
    }

    async fn modem_fw_version(&mut self) -> VersionString {
        MODEM_INFO.lock(|slot| slot.borrow().fw_version.clone())
    }
}
