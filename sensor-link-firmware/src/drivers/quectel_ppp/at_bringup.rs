//! AT-command bring-up for the PPP driver.
//!
//! Reuses the command definitions and modem-variant power sequencing of the
//! AT driver ([`crate::drivers::quectel`]) with its own, much smaller atat
//! resources: the AT phase here is a short sequential dialogue (power, baud,
//! SIM, APN, registration) that ends with `ATD*99#`, after which the UART
//! belongs to PPP.

use atat::asynch::AtatClient;
use atat::{AtatIngress, DefaultDigester, Ingress, ResponseSlot, UrcChannel, UrcSubscription};
use embedded_io_async::{Read, Write};
use sensor_link_protocol::info::VersionString;
use static_cell::StaticCell;

use crate::drivers::quectel::apn;
use crate::drivers::quectel::commands::{general, packetdomain, Urc};
use crate::drivers::quectel::variant::ModemVariant;
use crate::drivers::ATClient;
use crate::monotonic_time::{delay_ms, now, FutureTimeout};
use crate::traits::{BaudRateControl, Suspend};
use crate::utils::select::{select2, Select2};

/// Longest AT response line during bring-up (no payload-carrying commands).
pub(crate) const AT_INGRESS_BUF_SIZE: usize = 512;
const AT_CMD_BUF_SIZE: usize = 128;
const AT_URC_CAPACITY: usize = 2;
const AT_URC_SUBSCRIBERS: usize = 1;

const DEFAULT_BAUDRATE: u32 = 115_200;
/// Registration wait: matches the AT driver's CREG(90 s) + CEREG(60 s) budget.
const CREG_RETRIES: u32 = 90;
const CEREG_RETRIES: u32 = 60;
const DIAL_TIMEOUT_MS: u32 = 10_000;

static AT_RES_SLOT: ResponseSlot<AT_INGRESS_BUF_SIZE> = ResponseSlot::new();
static AT_URC_CHANNEL: UrcChannel<Urc, AT_URC_CAPACITY, AT_URC_SUBSCRIBERS> = UrcChannel::new();
static AT_INGRESS_BUF: StaticCell<[u8; AT_INGRESS_BUF_SIZE]> = StaticCell::new();
static AT_CMD_BUF: StaticCell<[u8; AT_CMD_BUF_SIZE]> = StaticCell::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BringupError {
    /// No AT response at any baudrate, or wrong modem model.
    Baudrate,
    /// SIM not ready.
    Sim,
    /// A configuration command failed.
    Command,
    /// The dial did not result in `CONNECT`.
    Dial,
}

/// Values captured during bring-up, served from cache while PPP is up.
#[derive(Default, Clone)]
pub struct ModemInfo {
    pub signal_quality: Option<i16>,
    pub model: VersionString,
    pub fw_version: VersionString,
}

/// One-time atat resources; owns the ingress and URC subscription for the
/// bring-up dialogues. Single instance (statics), like the AT driver.
pub(crate) struct AtEngine {
    ingress: Ingress<
        'static,
        DefaultDigester<Urc>,
        Urc,
        AT_INGRESS_BUF_SIZE,
        AT_URC_CAPACITY,
        AT_URC_SUBSCRIBERS,
    >,
    urc_subscription: UrcSubscription<'static, Urc, AT_URC_CAPACITY, AT_URC_SUBSCRIBERS>,
    cmd_buf: &'static mut [u8; AT_CMD_BUF_SIZE],
    baudrate_determined: Option<u32>,
}

pub(crate) struct BringupConfig {
    pub flow_control: bool,
    /// PDP context id used in `AT+CGDCONT` (PPP dials context 1).
    pub context_id: u8,
}

impl AtEngine {
    /// Panics if called twice: the atat buffers are statically allocated.
    pub fn take() -> Self {
        Self {
            ingress: Ingress::new(
                DefaultDigester::<Urc>::default(),
                AT_INGRESS_BUF.init([0; AT_INGRESS_BUF_SIZE]),
                &AT_RES_SLOT,
                &AT_URC_CHANNEL,
            ),
            urc_subscription: AT_URC_CHANNEL.subscribe().unwrap(),
            cmd_buf: AT_CMD_BUF.init([0; AT_CMD_BUF_SIZE]),
            baudrate_determined: None,
        }
    }

    /// Runs the full bring-up: power on, negotiate baudrate, SIM, APN,
    /// PDP context definition, network registration, `ATD*99#` dial.
    ///
    /// On `Ok` the modem is in PPP data mode and the UART carries PPP frames.
    /// Any PPP bytes that follow `CONNECT` immediately may be discarded; LCP
    /// retransmits its configure-requests, so the link recovers by itself.
    pub async fn bringup<W, R, V, U>(
        &mut self,
        tx: &mut W,
        rx: &mut R,
        uart: &mut U,
        power: &mut V,
        config: &BringupConfig,
    ) -> Result<ModemInfo, BringupError>
    where
        W: Write,
        R: Read,
        V: ModemVariant,
        U: Suspend + BaudRateControl,
    {
        let Self {
            ingress,
            urc_subscription,
            cmd_buf,
            baudrate_determined,
        } = self;

        // Power on before the sequence: the RDY URC needs the pump running.
        let was_off = !power.is_powered();
        if was_off {
            log::debug!(target: "quectel-ppp", "powering on modem");
            if !V::PERSIST_BAUDRATE {
                *baudrate_determined = None;
                uart.set_baud_rate(DEFAULT_BAUDRATE);
            }
            power.power_on().await;
        }
        uart.resume();

        let mut client: ATClient<'_, &mut W, AT_INGRESS_BUF_SIZE> =
            ATClient::new(tx, &AT_RES_SLOT, &mut cmd_buf[..]);

        let sequence = async {
            if was_off {
                // Wait for the boot URC; a timeout usually means the modem
                // was already on and booted long ago.
                match urc_subscription
                    .next_message_pure()
                    .with_timeout_ms(8_000)
                    .await
                {
                    Some(Urc::ModemReady) => log::debug!(target: "quectel-ppp", "modem ready"),
                    other => {
                        log::debug!(target: "quectel-ppp", "no RDY ({other:?}), assuming modem is on")
                    }
                }
            }

            let target = V::TARGET_BAUDRATE.unwrap_or(DEFAULT_BAUDRATE);
            match *baudrate_determined {
                Some(rate) => {
                    uart.set_baud_rate(rate);
                    delay_ms(1).await;
                    let _ = client.send(&general::AT).await;
                    if client.send(&general::AT).await.is_err() {
                        return Err(BringupError::Baudrate);
                    }
                }
                None => {
                    negotiate_baudrate::<_, V>(&mut client, uart, target).await?;
                    *baudrate_determined = Some(target);
                }
            }

            client
                .send(&general::ATE { value: 0 })
                .await
                .map_err(|_| BringupError::Command)?;

            if config.flow_control {
                client
                    .send(&general::SetFlowControl {
                        dce_by_dte: 2,
                        dte_by_dce: 2,
                    })
                    .await
                    .map_err(|_| BringupError::Command)?;
            }

            // SIM ready; the manual advises a reboot after ~20 s of retries.
            let mut retries = 20;
            while client.send(&general::CPIN).await.is_err() {
                retries -= 1;
                if retries == 0 {
                    return Err(BringupError::Sim);
                }
                delay_ms(1000).await;
            }

            // IMSI -> APN
            let access_point = match client.send(&general::CIMI).await {
                Ok(response) => match response.imsi_str() {
                    Ok(imsi) => apn::lookup(imsi).unwrap_or_else(|fallback| fallback),
                    Err(_) => apn::FALLBACK,
                },
                Err(_) => apn::FALLBACK,
            };
            log::info!(target: "quectel-ppp", "using APN '{access_point}'");

            client
                .send(&packetdomain::SetPDPContextDefinition {
                    cid: config.context_id.into(),
                    pdp_type: heapless::String::try_from("IP").unwrap(),
                    apn: heapless::String::try_from(access_point)
                        .map_err(|_| BringupError::Command)?,
                })
                .await
                .map_err(|_| BringupError::Command)?;

            wait_for_registration(&mut client).await;

            let mut info = ModemInfo::default();
            if let Ok(report) = client.send(&general::GetSignalQuality).await {
                info.signal_quality = Some(report.signal_strength());
            }
            if let Ok(model) = client.send(&general::GetModelId).await {
                info.model = into_version_string(&model.id);
            }
            if let Ok(version) = client.send(&general::GetSoftwareVersion).await {
                info.fw_version = into_version_string(&version.id);
            }
            Ok(info)
        };

        // The ingress pump runs only while the sequence does; afterwards the
        // UART is handed to the dial + PPP.
        let info = match select2(sequence, pump(ingress, rx)).await {
            Select2::A(result) => result?,
            Select2::B(_) => unreachable!("pump never completes"),
        };

        dial(tx, rx).await?;
        Ok(info)
    }

    /// Best-effort clean shutdown after PPP: escape to command mode and send
    /// AT+QPOWD. The caller powers off the modem afterwards.
    pub async fn shutdown<W, R>(&mut self, tx: &mut W, rx: &mut R)
    where
        W: Write,
        R: Read,
    {
        escape_to_command_mode(tx).await;
        let Self { ingress, cmd_buf, .. } = self;
        let mut client: ATClient<'_, &mut W, AT_INGRESS_BUF_SIZE> =
            ATClient::new(tx, &AT_RES_SLOT, &mut cmd_buf[..]);
        let sequence = async {
            let _ = client.send(&general::PowerDown::new()).await;
        };
        match select2(sequence, pump(ingress, rx)).await {
            Select2::A(()) => {}
            Select2::B(_) => unreachable!("pump never completes"),
        }
    }
}

/// Feeds UART bytes into the atat ingress. Never completes; run under select.
async fn pump<R: Read>(
    ingress: &mut Ingress<
        'static,
        DefaultDigester<Urc>,
        Urc,
        AT_INGRESS_BUF_SIZE,
        AT_URC_CAPACITY,
        AT_URC_SUBSCRIBERS,
    >,
    rx: &mut R,
) {
    loop {
        let buf = ingress.write_buf();
        match rx.read(buf).await {
            Ok(n) if n > 0 => ingress.advance(n).await,
            Ok(_) => {}
            Err(_) => {
                ingress.clear();
                // Reading again immediately would spin on a persistent
                // error; errors here end the bring-up via command timeouts.
                delay_ms(10).await;
            }
        }
    }
}

async fn negotiate_baudrate<W: Write, V: ModemVariant>(
    client: &mut ATClient<'_, &mut W, AT_INGRESS_BUF_SIZE>,
    uart: &mut impl BaudRateControl,
    target: u32,
) -> Result<(), BringupError> {
    uart.set_baud_rate(target);
    delay_ms(1).await;
    let _ = client.send(&general::AT).await;
    if client.send(&general::AT).await.is_ok() {
        return verify_model::<_, V>(client).await;
    }

    uart.set_baud_rate(DEFAULT_BAUDRATE);
    delay_ms(1).await;
    let _ = client.send(&general::AT).await;
    if client.send(&general::AT).await.is_err() {
        return Err(BringupError::Baudrate);
    }

    if client
        .send(&general::SetBaudRate { rate: target })
        .await
        .is_ok()
    {
        uart.set_baud_rate(target);
    }
    delay_ms(500).await;
    let _ = client.send(&general::AT).await;
    if client.send(&general::AT).await.is_err() {
        return Err(BringupError::Baudrate);
    }
    verify_model::<_, V>(client).await
}

async fn verify_model<W: Write, V: ModemVariant>(
    client: &mut ATClient<'_, &mut W, AT_INGRESS_BUF_SIZE>,
) -> Result<(), BringupError> {
    let model = client
        .send(&general::GetModelId)
        .await
        .map_err(|_| BringupError::Baudrate)?;
    let matches = model
        .id
        .get(..V::MODEL_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(V::MODEL_PREFIX));
    if matches {
        Ok(())
    } else {
        log::error!(target: "quectel-ppp", "unexpected modem model {:?}", model.id);
        Err(BringupError::Baudrate)
    }
}

async fn wait_for_registration<W: Write>(client: &mut ATClient<'_, &mut W, AT_INGRESS_BUF_SIZE>) {
    let mut left = CREG_RETRIES;
    while left > 0 {
        match client.send(&general::CREGQuery).await {
            Ok(resp) if resp.stat == 1 || resp.stat == 5 => break,
            _ => {
                left -= 1;
                delay_ms(1000).await;
            }
        }
    }
    let mut left = CEREG_RETRIES;
    while left > 0 {
        match client.send(&general::CEREGQuery).await {
            Ok(resp) if resp.stat == 1 || resp.stat == 5 => break,
            _ => {
                left -= 1;
                delay_ms(1000).await;
            }
        }
    }
}

/// Dials PPP data mode. Raw (non-atat): the result code is `CONNECT`, which
/// the OK/ERROR digester cannot represent.
async fn dial<W: Write, R: Read>(tx: &mut W, rx: &mut R) -> Result<(), BringupError> {
    tx.write_all(b"ATD*99#\r")
        .await
        .map_err(|_| BringupError::Dial)?;
    tx.flush().await.map_err(|_| BringupError::Dial)?;

    let deadline = now().add_micros(u64::from(DIAL_TIMEOUT_MS) * 1000);
    let mut window = heapless::Vec::<u8, 256>::new();
    loop {
        let remaining_ms = deadline.micros_since(&now()) / 1000;
        if remaining_ms == 0 {
            return Err(BringupError::Dial);
        }
        let mut chunk = [0u8; 64];
        let n = match rx
            .read(&mut chunk)
            .with_timeout_ms(remaining_ms as u32)
            .await
        {
            Some(Ok(n)) => n,
            Some(Err(_)) | None => return Err(BringupError::Dial),
        };
        for &byte in &chunk[..n] {
            if window.is_full() {
                window.remove(0);
            }
            let _ = window.push(byte);
        }
        if find(&window, b"CONNECT").is_some() {
            return Ok(());
        }
        if find(&window, b"NO CARRIER").is_some() || find(&window, b"ERROR").is_some() {
            return Err(BringupError::Dial);
        }
    }
}

/// `+++` escape from PPP data mode back to AT command mode: one second of
/// guard silence on both sides of the escape sequence.
pub(crate) async fn escape_to_command_mode<W: Write>(tx: &mut W) {
    delay_ms(1100).await;
    let _ = tx.write_all(b"+++").await;
    let _ = tx.flush().await;
    delay_ms(1100).await;
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn into_version_string(id: &str) -> VersionString {
    VersionString::try_from(id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{BaudRateControl, Suspend};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::vec::Vec;

    struct SimState {
        /// (expected write prefix, canned response bytes)
        script: VecDeque<(&'static str, &'static [u8])>,
        rx_queue: VecDeque<u8>,
        line: Vec<u8>,
    }

    #[derive(Clone)]
    struct Sim(Rc<RefCell<SimState>>);

    impl Sim {
        fn new(script: &[(&'static str, &'static [u8])]) -> Self {
            Sim(Rc::new(RefCell::new(SimState {
                script: script.iter().copied().collect(),
                rx_queue: VecDeque::new(),
                line: Vec::new(),
            })))
        }

        fn done(&self) -> bool {
            self.0.borrow().script.is_empty()
        }
    }

    impl embedded_io_async::ErrorType for Sim {
        type Error = core::convert::Infallible;
    }

    impl embedded_io_async::Write for Sim {
        async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
            let mut state = self.0.borrow_mut();
            state.line.extend_from_slice(data);
            if state.line.ends_with(b"\r") {
                let line = core::mem::take(&mut state.line);
                let line_str = String::from_utf8_lossy(&line).into_owned();
                let (expected, response) = state
                    .script
                    .pop_front()
                    .unwrap_or_else(|| panic!("unexpected extra command: {line_str:?}"));
                assert!(
                    line_str.starts_with(expected),
                    "expected command starting with {expected:?}, got {line_str:?}"
                );
                state.rx_queue.extend(response.iter().copied());
            }
            Ok(data.len())
        }
        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl embedded_io_async::Read for Sim {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            loop {
                {
                    let mut state = self.0.borrow_mut();
                    if !state.rx_queue.is_empty() {
                        let mut n = 0;
                        while n < buf.len() {
                            match state.rx_queue.pop_front() {
                                Some(b) => {
                                    buf[n] = b;
                                    n += 1;
                                }
                                None => break,
                            }
                        }
                        return Ok(n);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        }
    }

    struct SimVariant;
    impl ModemVariant for SimVariant {
        const MODEL_PREFIX: &'static str = "eg915";
        const TARGET_BAUDRATE: Option<u32> = None;
        const PERSIST_BAUDRATE: bool = true;
        fn is_powered(&mut self) -> bool {
            true
        }
        async fn power_on(&mut self) {}
        fn power_off(&mut self) {}
    }

    struct NopUart;
    impl Suspend for NopUart {
        fn suspend(&mut self) {}
        fn resume(&mut self) {}
        fn is_suspended(&self) -> bool {
            false
        }
    }
    impl BaudRateControl for NopUart {
        fn set_baud_rate(&mut self, _: u32) {}
        fn get_baud_rate(&self) -> u32 {
            DEFAULT_BAUDRATE
        }
    }

    const OK: &[u8] = b"\r\nOK\r\n";

    /// One combined test: `AtEngine::take` uses statics and can only run once
    /// per process.
    #[tokio::test]
    async fn bringup_dialogue_and_dial() {
        simple_logger::init().ok();
        crate::std_monotonic_driver::start();
        let mut engine = AtEngine::take();

        // Happy path up to and including the dial.
        let sim = Sim::new(&[
            ("AT\r", OK),
            ("AT\r", OK),
            ("AT+CGMM\r", b"\r\nEG915NEUAC\r\n\r\nOK\r\n"),
            ("ATE0\r", OK),
            ("AT+CPIN?\r", b"\r\n+CPIN: READY\r\n\r\nOK\r\n"),
            ("AT+CIMI\r", b"\r\n204080000000000\r\n\r\nOK\r\n"),
            ("AT+CGDCONT=1,\"IP\",\"", OK),
            ("AT+CREG?\r", b"\r\n+CREG: 0,1\r\n\r\nOK\r\n"),
            ("AT+CEREG?\r", b"\r\n+CEREG: 0,1\r\n\r\nOK\r\n"),
            ("AT+CSQ\r", b"\r\n+CSQ: 20,99\r\n\r\nOK\r\n"),
            ("AT+CGMM\r", b"\r\nEG915NEUAC\r\n\r\nOK\r\n"),
            ("AT+CGMR\r", b"\r\nEG915NEUACR03A03M08\r\n\r\nOK\r\n"),
            // PPP bytes right after CONNECT must not confuse the dial.
            ("ATD*99#\r", b"\r\nCONNECT 150000000\r\n\x7e\xff\x7d\x23\xc0\x21"),
        ]);
        let (mut tx, mut rx) = (sim.clone(), sim.clone());
        let info = engine
            .bringup(
                &mut tx,
                &mut rx,
                &mut NopUart,
                &mut SimVariant,
                &BringupConfig {
                    flow_control: false,
                    context_id: 1,
                },
            )
            .await
            .expect("bringup failed");
        assert!(sim.done(), "not all scripted commands were sent");
        assert!(info.signal_quality.is_some());
        assert!(info.model.as_str().to_ascii_lowercase().contains("eg915"));

        // Dial rejection: NO CARRIER must fail, not hang.
        let sim = Sim::new(&[("ATD*99#\r", b"\r\nNO CARRIER\r\n")]);
        let (mut tx, mut rx) = (sim.clone(), sim.clone());
        let result = dial(&mut tx, &mut rx).await;
        assert_eq!(result, Err(BringupError::Dial));

        // CONNECT split across reads.
        let sim = Sim::new(&[("ATD*99#\r", b"\r\nCONN")]);
        {
            let mut state = sim.0.borrow_mut();
            state.rx_queue.extend(b"".iter().copied());
        }
        let (mut tx, mut rx) = (sim.clone(), sim.clone());
        let dial_fut = dial(&mut tx, &mut rx);
        let feeder = async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            sim.0.borrow_mut().rx_queue.extend(b"ECT\r\n".iter().copied());
        };
        let (result, ()) = tokio::join!(dial_fut, feeder);
        assert_eq!(result, Ok(()));
    }
}
