//! Quectel modem driver (EC21/EC25/EG915N)

mod apn;
mod commands;
mod config;
mod types;
mod variant;

use core::{
    marker::PhantomData,
    sync::atomic::{AtomicBool, Ordering},
};

use atat::{
    asynch::AtatClient, AtatIngress, DefaultDigester, Ingress, UrcChannel, UrcSubscription,
};
use embedded_io_async::{Read, Write};
use heapless::{spsc, String, Vec};
use sensor_link_protocol::{
    info::{VersionString, VERSION_STRING_MAX_LEN},
    Error, MAX_FILE_CHUNK_LEN, MAX_MESSAGE_LEN, MAX_TOPIC_LEN,
};
use static_cell::StaticCell;

use crate::{
    drivers::{quectel::commands::mqtt::urc::MQTTReceive, ATClient},
    monotonic_time::{delay_ms, FutureTimeout},
    mqtt::{Event, FileError, MqttClient, Will},
    traits::{BaudRateControl, ErrorReport, Split, Suspend, Suspendable},
};

use commands::{file::responses::Upload, *};
use types::*;

pub use config::Config;
pub use variant::{Eg915, MiniPcie, ModemVariant};

use self::mqtt::urc::MqttAck;

const INGRESS_BUF_SIZE: usize = 2048;
const URC_CAPACITY: usize = 128;
const URC_SUBSCRIBERS: usize = 1;
const URC_QUEUE_LEN: usize = 6; // EC2x and EG915N have 5 slots (0-4) for receiving mqtt messages.
const MQTT_CONTEXT: ContextId = ContextId(1);
const TLS_CONTEXT: ContextId = ContextId(2);
const HTTP_CONTEXT: ContextId = ContextId(3);

const TEST_FILE: &str = "Test File 123!\n";
const RAM_TMP_FILE: &str = "RAM:tmp.file";
const CA_FILE: &str = "UFS:cacert.pem";
const CLIENT_CERT_FILE: &str = "UFS:client.crt";
const CLIENT_KEY_FILE: &str = "UFS:client.key";

static RES_SLOT: atat::ResponseSlot<INGRESS_BUF_SIZE> = atat::ResponseSlot::new();
static URC_CHANNEL: atat::UrcChannel<Urc, URC_CAPACITY, URC_SUBSCRIBERS> = UrcChannel::new();

/// Must be >= the largest AT command (AtatCmd::MAX_LEN).
/// Note: this is not necessarily the same as sensor_link_protocol::MAX_MESSAGE_LEN since
/// publish data does not use the command buffer..
const CMD_BUF_SIZE: usize = 1548;
static ERROR_OCCURRED: AtomicBool = AtomicBool::new(false);
static INGRESS_BUF: StaticCell<[u8; INGRESS_BUF_SIZE]> = StaticCell::new();
static CMD_BUF: StaticCell<[u8; CMD_BUF_SIZE]> = StaticCell::new();

pub const DEFAULT_BAUDRATE: u32 = 115200;

pub struct Credentials {
    pub ca_cert: &'static str,
    pub client_cert: &'static [u8],
    pub client_key: &'static [u8],
}

/// Driver for Quectel EC21/EC25/EG915N modems.
pub struct Quectel<W: Write, R: Read + ErrorReport + Suspendable, V: ModemVariant, U: Suspend> {
    _phantom: PhantomData<R>,
    client: ATClient<'static, W, INGRESS_BUF_SIZE>,
    urc_subscription: UrcSubscription<'static, Urc, URC_CAPACITY, URC_SUBSCRIBERS>,
    unexpected_urc_queue: spsc::Queue<Urc, URC_QUEUE_LEN>,

    power: V,
    uart: U,
    config: Config,
    /// During first boot we need to do some
    /// extra initialization.
    first_boot: bool,

    apn: apn::APN,

    baudrate_determined: Option<u32>,

    credentials: Credentials,

    // State
    /// Indicates that we might have missed messages.
    packets_missed: bool,

    /// Keeps track of last received message id
    /// Can be used to detect missed messages
    last_rx_msg_id: u16,

    /// Keeps track of last sent message id
    /// can be used to match acks (QOS>1)
    next_tx_msg_id: u16,

    file_handle: Option<u32>,

    /// URC that needs to be handled
    urc: Option<Urc>,
}

pub struct QuectelBackend<R: Read + ErrorReport + Suspendable> {
    ingress: Ingress<
        'static,
        DefaultDigester<Urc>,
        Urc,
        INGRESS_BUF_SIZE,
        URC_CAPACITY,
        URC_SUBSCRIBERS,
    >,
    rx: R,
}

impl<W, R, V, U> Quectel<W, R, V, U>
where
    W: Write,
    V: ModemVariant,
    R: Read + ErrorReport + Suspendable,
    U: Suspend + BaudRateControl,
{
    /// Note: only a single instance per firmware image is supported,
    /// as the atat buffers and channels are statically allocated.
    pub fn new(
        uart: impl Split<W, R, U>,
        power: V,
        config: Config,
        credentials: Credentials,
    ) -> (Self, QuectelBackend<R>) {
        let ingress = Ingress::new(
            DefaultDigester::<Urc>::default(),
            INGRESS_BUF.init([0; INGRESS_BUF_SIZE]),
            &RES_SLOT,
            &URC_CHANNEL,
        );

        let (uart, tx, rx) = uart.split();

        let client: ATClient<'_, W, INGRESS_BUF_SIZE> =
            ATClient::new(tx, &RES_SLOT, CMD_BUF.init([0; CMD_BUF_SIZE]));
        let urc_subscription = URC_CHANNEL.subscribe().unwrap();

        (
            Self {
                _phantom: PhantomData,
                client,
                urc_subscription,
                unexpected_urc_queue: spsc::Queue::new(),

                power,
                config,
                uart,
                first_boot: true,
                apn: apn::FALLBACK,
                baudrate_determined: if V::TARGET_BAUDRATE.is_none() {
                    // No negotiation for this variant: stay at the default baudrate
                    Some(DEFAULT_BAUDRATE)
                } else {
                    None
                },
                credentials,

                packets_missed: false,
                last_rx_msg_id: 0,
                next_tx_msg_id: 0,
                file_handle: None,
                urc: None,
            },
            QuectelBackend { ingress, rx },
        )
    }

    pub async fn initialize(&mut self) -> Result<(), Error<atat::Error>> {
        // Turn on modem only if it is not already on
        self.turn_on().await;

        if let Some(baudrate) = self.baudrate_determined {
            log::debug!(target: "quectel","Set baudrate to {baudrate}");
            self.uart.set_baud_rate(baudrate);
            delay_ms(1).await;

            let cmd = general::AT;
            let _ = self.client.send(&cmd).await;
            if self.client.send(&cmd).await.is_err() {
                log::error!(target: "quectel","AT failed");
                return Err(Error::TimeOut);
            }
        } else {
            let target = V::TARGET_BAUDRATE.unwrap_or(DEFAULT_BAUDRATE);
            match self.negotiate_baudrate(target, DEFAULT_BAUDRATE).await {
                Ok(_) => self.baudrate_determined = Some(target),
                Err(_) => {
                    log::warn!(target: "quectel","Baudrate not as expected. Back to default");
                    self.baudrate_determined = Some(DEFAULT_BAUDRATE);
                    self.uart.set_baud_rate(DEFAULT_BAUDRATE);
                    return Err(Error::TimeOut);
                }
            }
        }

        let cmd = general::ATE { value: 0 };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // let cmd = general::GetManufacturerId;
        // let manuf_id = self.client.send(&cmd).await.map_err(Error::Client)?;
        // log::debug!(target: "quectel","ID: {manuf_id:?}");

        // let cmd = general::GetSoftwareVersion;
        // let version = self.client.send(&cmd).await.map_err(Error::Client)?;
        // log::debug!(target: "quectel","Software version: {version:?}");

        let cpin_cmd = general::CPIN;
        let mut retries = 20; // Manual says to reboot after 20 secs
        while let Err(err) = self.client.send(&cpin_cmd).await {
            delay_ms(1000).await;
            retries -= 1;
            if retries == 0 {
                return Err(Error::Client(err));
            }

            let cmd = general::ATE { value: 0 };
            self.client.send(&cmd).await.map_err(Error::Client)?;
        }

        self.try_autodetect_apn().await;

        if self.first_boot {
            match self.ensure_provisioned_all().await {
                // Provisioning succesfull: toggle flag to skip it next time
                Ok(_) => self.first_boot = false,

                Err(orig_error) => {
                    match orig_error {
                        // Timeout: try to detect if the modem is still in usable state.
                        // If so, ignore and assume the provisioning was probably succesfull sometime earlier.
                        Error::Client(atat::Error::Timeout) => {
                            let mut model = self.modem_model().await;
                            model.make_ascii_lowercase();

                            if !model.contains(V::MODEL_PREFIX) {
                                log::warn!(target: "quectel", "provisioning failed (modem model detected as '{model}')");
                                return Err(orig_error);
                            }
                        }
                        // Ignore errors, assume provisioning probably succeeded some time earlier
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Ensure all required files are provisioned into the modem
    async fn ensure_provisioned_all(&mut self) -> Result<(), Error<atat::Error>> {
        self.ensure_provisioned(CA_FILE, self.credentials.ca_cert.as_bytes())
            .await?;
        self.ensure_provisioned(CLIENT_CERT_FILE, self.credentials.client_cert)
            .await?;
        self.ensure_provisioned(CLIENT_KEY_FILE, self.credentials.client_key)
            .await
    }

    async fn negotiate_baudrate(
        &mut self,
        target_baudrate: u32,
        default_baudrate: u32,
    ) -> Result<(), Error<atat::Error>> {
        // Try to communicate at the target baudrate
        log::debug!(target: "quectel","Negotiate baudrate");
        self.uart.set_baud_rate(target_baudrate);

        delay_ms(1).await;

        let cmd = general::AT;
        // First a dummy command to flush any bit errors
        let _ = self.client.send(&cmd).await;

        if self.client.send(&cmd).await.is_ok() {
            log::debug!(target: "quectel","Baudrate negotiated to {target_baudrate}");
            return Ok(());
        } else {
            log::warn!(target: "quectel","Baudrate not as expected");
        };

        // Maybe modem is still at default baudrate.
        log::debug!(target: "quectel","Set baudrate to default ({default_baudrate})");
        self.uart.set_baud_rate(default_baudrate);
        delay_ms(1).await;

        let cmd = general::AT;
        // First a dummy command to flush any bit errors
        let _ = self.client.send(&cmd).await;

        if self.client.send(&cmd).await.is_ok() {
            log::debug!(target: "quectel","Baudrate currently {default_baudrate}");
        } else {
            log::error!(target: "quectel","Failed to negotiate baudrate");
            return Err(Error::TimeOut);
        };

        let cmd = general::SetBaudRate {
            rate: target_baudrate,
        };
        if self.client.send(&cmd).await.is_ok() {
            log::debug!(target: "quectel","Try setting Baudrate to {target_baudrate}");
            self.uart.set_baud_rate(target_baudrate);
        }

        delay_ms(500).await;

        let cmd = general::AT;

        // First a dummy command to flush any bit errors
        let _ = self.client.send(&cmd).await;

        // Test if we can parse the response
        if self.client.send(&cmd).await.is_ok() {
            // Extra check
            let cmd = general::GetModelId;
            let model_id = self.client.send(&cmd).await.map_err(Error::Client)?;
            log::info!(target: "quectel","ID: {model_id:?}");

            // Verify id.
            let matches_model = model_id
                .id
                .get(..V::MODEL_PREFIX.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(V::MODEL_PREFIX));
            if !matches_model {
                log::error!(target: "quectel","Failed to negotiate baudrate 2");
                return Err(Error::TimeOut); // TODO Timeout Error is not super correct
            }
            log::debug!(target: "quectel","Baudrate negotiated to {target_baudrate}");
        } else {
            log::error!(target: "quectel","Failed to negotiate baudrate 1");
            return Err(Error::TimeOut);
        };

        delay_ms(1).await;

        // Now we're pretty sure the baudrates agree. We can persist the settings

        let cmd = general::StoreBaudrate;
        if let Err(err) = self.client.send(&cmd).await.map_err(Error::Client) {
            log::error!(target: "quectel","Failed to store baudrate: {err:?}");
            // We can try again next power cycle.
        }

        log::info!(target: "quectel","Baudrate stored in modem");

        Ok(())
    }

    async fn try_autodetect_apn(&mut self) {
        // Try to get IMSI from modem
        let cimi_resp = self.client.send(&general::CIMI).await;
        let imsi = match cimi_resp {
            Ok(ref response) => response.imsi_str(),
            Err(err) => Err(err),
        };

        self.apn = match imsi {
            // Fallback
            Err(err) => {
                log::warn!("Failed to detect IMSI: {err:?}. Trying fallback APN...");
                apn::FALLBACK
            }

            // Try to lookup APN from IMSI
            Ok(imsi) => match apn::lookup(imsi) {
                Ok(apn) => apn,
                Err(fallback) => {
                    log::warn!("Failed to lookup APN. Trying fallback APN...");
                    fallback
                }
            },
        };

        log::info!("Using APN '{}'", self.apn);
    }

    async fn ensure_provisioned(
        &mut self,
        filename: &str,
        reference_content: &[u8],
    ) -> Result<(), Error<atat::Error>> {
        if reference_content.is_empty() {
            log::debug!(target: "quectel","Skipping provisioning of {filename}: is empty");
            return Ok(()); // skip
        }
        // Check if file needs updating
        let mut offset = 0usize;
        let mut needs_update = false;
        while offset < reference_content.len() {
            if let Some(content) = &self.read_file_chunk(filename, MAX_FILE_CHUNK_LEN).await? {
                let end = offset + content.len();
                if end > reference_content.len() {
                    // Size mismatch
                    needs_update = true;
                    break;
                }
                let reference = &reference_content[offset..end];
                if content != reference {
                    needs_update = true;
                    break;
                }
                offset += content.len();
            } else {
                // End of file.
                if offset < reference_content.len() {
                    needs_update = true;
                }
                break;
            }
        }

        let _ = self.close_file().await;

        if needs_update {
            log::debug!(target: "quectel","Updating {filename}");
            let _ = self.delete_file(filename).await;
            self.upload_file(filename, reference_content).await?;
        } else {
            log::debug!(target: "quectel","{filename} is up to date");
        }

        Ok(())
    }

    pub async fn get_signal_quality(&mut self) -> Result<i16, Error<atat::Error>> {
        let cmd = general::GetSignalQuality;
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        Ok(resp.signal_strength())
    }

    pub async fn upload_file(
        &mut self,
        filename: &str,
        contents: &[u8],
    ) -> Result<(), Error<atat::Error>> {
        log::debug!(target: "quectel","Upload file: {}", filename);
        let filename = String::try_from(filename).map_err(|_| Error::Serialize)?;

        let cmd = file::UploadInit {
            filename,
            file_size: contents.len() as u32,
            timeout: 60,
        };
        log::debug!(target: "quectel","Wait for CONNECT");
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let upload_result: Upload = self
            .client
            .send_bytes(contents, 300_000)
            .await
            .map_err(Error::Client)?;
        if contents.len() != upload_result.size as usize {
            return Err(Error::Serialize);
        }

        log::debug!(target: "quectel","File upload done");

        Ok(())
    }

    /// Download a file with a new https context
    pub async fn download_file(&mut self, url: &str) -> Result<(), Error<atat::Error>> {
        let cmd = http::ConfigContext::pdp(HTTP_CONTEXT);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Do not output HTTP response header.
        let cmd = http::ConfigResponseHeader::new(false);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Configure PDP context
        let cmd = packetdomain::SetPDPContextDefinition {
            cid: HTTP_CONTEXT,
            pdp_type: String::try_from("IP").unwrap(),
            apn: String::try_from(self.apn).unwrap(),
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Check CREG
        let cmd = general::CREGQuery;
        let mut retries = 90;
        while retries > 0 {
            match self.client.send(&cmd).await {
                Ok(resp) if resp.stat == 1 || resp.stat == 5 => {
                    log::debug!(target: "quectel","Network registered");
                    break;
                }
                _ => {
                    retries -= 1;
                    delay_ms(1000).await;
                }
            };
        }

        // Check CEREG
        let cmd = general::CEREGQuery;
        let mut retries = 60;
        while retries > 0 {
            match self.client.send(&cmd).await {
                Ok(resp) if resp.stat == 1 || resp.stat == 5 => {
                    log::debug!(target: "quectel","LTE registered");
                    break;
                }
                _ => {
                    retries -= 1;
                    delay_ms(1000).await;
                }
            };
        }

        // Activate context
        let cmd = tcpip::ActivateContext { cid: HTTP_CONTEXT };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Set SSL context ID
        let cmd = http::ConfigContext::ssl(HTTP_CONTEXT);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Allow all SSL/TLS versions
        let cmd = ssl::SSLCFG::sslversion(HTTP_CONTEXT, 4);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Do not check server certificate
        let cmd = ssl::SSLCFG::seclevel(HTTP_CONTEXT, 0);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Allow all ciphersuites
        let cmd = ssl::SSLCFGCiphersuiteFixed1;
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let cmd = http::URL {
            url_length: url.len() as u16,
            timeout: 20,
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;
        self.client
            .send_bytes::<NoResponse>(url.as_bytes(), 100)
            .await
            .map_err(Error::Client)?;

        log::debug!(target: "quectel","URL Ok");

        let cmd = http::GET { timeout: 10 };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        if let Some(Urc::HTTPGet(resp)) = self.await_urc(UrcVariant::HTTPGet).await {
            match resp.resp {
                Some(200) => {
                    log::debug!(target: "quectel","GET Ok");
                }
                Some(code) => {
                    log::error!(target: "quectel","GET Error: {}", code);
                    return Err(Error::Serialize); // todo better error
                }
                None => {
                    log::error!(target: "quectel","GET Error: {}", resp.err);
                    return Err(Error::Serialize); // todo better error
                }
            }
        } else {
            log::error!(target: "quectel","GET URC not received");
        }

        let cmd = http::ReadFile {
            file_name: String::try_from("RAM:tmp.file").unwrap(),
            wait_time: 10,
        };

        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","Read to file: {:?}", resp);

        if let Some(Urc::HTTPReadFile(resp)) = self.await_urc(UrcVariant::HTTPReadFile).await {
            if resp.err == 0 {
                log::debug!(target: "quectel","Read to File Ok: {:?}", resp);
            } else {
                log::error!(target: "quectel","Read to File Error: {:?}", resp);
            }
        } else {
            log::error!(target: "quectel","Read to File URC not received");
        }

        // We're done, deactivate this context.

        self.client
            .send(&tcpip::DeactivateContext { cid: HTTP_CONTEXT })
            .await
            .map(|_| ())
            .map_err(|err| {
                log::error!(target: "quectel","Failed to deactivate context: {err:?}");
                Error::MQTT("Deactivate Context")
            })
    }

    /// Read a file chunk from the modem filesystem.
    /// Opens the file if it is not already open.
    /// Subsequent calls will read from the same file from the next offset.
    /// File should be closed manually by calling `close_file()`
    async fn read_file_chunk(
        &mut self,
        filename: &str,
        chunk_size: usize,
    ) -> Result<Option<Vec<u8, MAX_FILE_CHUNK_LEN>>, Error<atat::Error>> {
        if chunk_size > MAX_FILE_CHUNK_LEN {
            return Err(Error::Serialize); // todo wrong error variant
        }
        if self.file_handle.is_none() {
            let filename = String::try_from(filename).map_err(|_| Error::Serialize)?;
            let cmd = file::Open { filename, mode: 0 };
            let resp = self.client.send(&cmd).await.map_err(Error::Client)?; // Todo FileNotFound error
            self.file_handle = Some(resp.handle);
            log::debug!(target: "quectel","File open OK");

            let cmd = file::Seek {
                handle: resp.handle,
                offset: 0,
                position: 0,
            };
            self.client.send(&cmd).await.map_err(Error::Client)?;
            log::debug!(target: "quectel","File seek OK");
        }

        // If we have a file_handle we assume the file is open.
        if let Some(handle) = self.file_handle {
            let cmd = file::Read {
                handle,
                len: chunk_size as u32,
            };

            match self.client.send(&cmd).await {
                Ok(resp) => {
                    log::debug!(target: "quectel","File read OK: {:?}", resp.bytes.len());
                    let mut new_vec = Vec::new();
                    // Todo: We cannot into() the bytes because of an old version of heapless-bytes
                    // See https://github.com/ycrypto/heapless-bytes/pull/3.
                    let n = resp.bytes.len().min(chunk_size);
                    // Unwrap: We know it will fit as the length was checked.
                    new_vec.extend_from_slice(&resp.bytes[..n]).unwrap();
                    return Ok(Some(new_vec));
                }
                Err(_err) => return Ok(None),
            }
        }

        Ok(None)
    }

    /// Close the currently opened file. Error is file does not exist.
    async fn close_file(&mut self) -> Result<(), Error<atat::Error>> {
        if let Some(handle) = self.file_handle.take() {
            let cmd = file::Close { handle };
            self.client.send(&cmd).await.map_err(Error::Client)?;
        }
        Ok(())
    }

    /// Deletes a file. NB: File must be closed.
    async fn delete_file(&mut self, filename: &str) -> Result<(), Error<atat::Error>> {
        log::debug!(target: "quectel","Delete file: {}", filename);
        let filename = String::try_from(filename).map_err(|_| Error::Serialize)?;
        let cmd = file::Delete { filename };
        self.client.send(&cmd).await.map_err(Error::Client)?;
        Ok(())
    }

    pub async fn test_http_get(&mut self) -> Result<(), Error<atat::Error>> {
        log::debug!(target: "quectel","Start http commands");
        let cmd = http::ConfigContext::pdp(1);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let cmd = http::ConfigResponseHeader::new(true);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Configure PDP context
        let cmd = packetdomain::SetPDPContextDefinition {
            cid: HTTP_CONTEXT,
            pdp_type: String::try_from("IP").unwrap(),
            apn: String::try_from(self.apn).unwrap(),
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Check CREG
        let cmd = general::CREGQuery;
        let mut retries = 90;
        while retries > 0 {
            match self.client.send(&cmd).await {
                Ok(resp) if resp.stat == 1 || resp.stat == 5 => {
                    log::debug!(target: "quectel","Network registered");
                    break;
                }
                _ => {
                    retries -= 1;
                    delay_ms(1000).await;
                }
            };
        }

        // Check CEREG
        let cmd = general::CEREGQuery;
        let mut retries = 60;
        while retries > 0 {
            match self.client.send(&cmd).await {
                Ok(resp) if resp.stat == 1 || resp.stat == 5 => {
                    log::debug!(target: "quectel","LTE registered");
                    break;
                }
                _ => {
                    retries -= 1;
                    delay_ms(1000).await;
                }
            };
        }

        // Activate context
        let cmd = tcpip::ActivateContext { cid: HTTP_CONTEXT };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // AT+QSSLCFG="seclevel",1,0
        let cmd = ssl::SSLCFG::seclevel(HTTP_CONTEXT, 0);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // AT+QSSLCFG="ciphersuite",1,0xFFFF
        let cmd = ssl::SSLCFGCiphersuiteFixed1;
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // AT+QSSLCFG="seclevel"
        let cmd = ssl::SSLCFG::seclevel(HTTP_CONTEXT, 0);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        //
        let cmd = ssl::SSLCFG::sslversion(HTTP_CONTEXT, 1);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let url: String<20> = String::try_from("https://jitter.nl").unwrap();
        let cmd = http::URL {
            url_length: url.len() as u16,
            timeout: 20,
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // wait for connect
        // Send url data.
        self.client
            .send_bytes::<NoResponse>(url.as_bytes(), 100)
            .await
            .map_err(Error::Client)?;

        log::debug!(target: "quectel","URL Ok");

        let cmd = http::GET { timeout: 10 };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        if let Some(Urc::HTTPGet(resp)) = self.await_urc(UrcVariant::HTTPGet).await {
            if resp.err == 0 {
                log::debug!(target: "quectel","GET Ok: {:?}", resp);
            } else {
                log::error!(target: "quectel","GET Error: {:?}", resp);
            }
        } else {
            log::error!(target: "quectel","GET URC not received");
        }

        let cmd = http::ReadFile {
            file_name: String::try_from("RAM:testget.txt").unwrap(),
            wait_time: 10,
        };

        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","Read to file: {:?}", resp);

        if let Some(Urc::HTTPReadFile(resp)) = self.await_urc(UrcVariant::HTTPReadFile).await {
            if resp.err == 0 {
                log::debug!(target: "quectel","Read to File Ok: {:?}", resp);
            } else {
                log::error!(target: "quectel","Read to File Error: {:?}", resp);
            }
        } else {
            log::error!(target: "quectel","Read to File URC not received");
        }

        let cmd = file::List {
            pattern: String::try_from("RAM:testget.txt").unwrap(),
        };
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","File list: {:?}", resp);

        log::debug!(target: "quectel","Demo done!");

        Ok(())
    }

    pub async fn test_read_file(&mut self) -> Result<(), Error<atat::Error>> {
        log::debug!(target: "quectel","Start file commands");

        delay_ms(3000).await;

        let cmd = file::Space {
            pattern: String::try_from("UFS").unwrap(),
        };
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","UFS: {:?}", resp);

        let cmd = file::Space {
            pattern: String::try_from("RAM").unwrap(),
        };
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","RAM: {:?}", resp);

        let cmd = file::List {
            pattern: String::try_from(CLIENT_CERT_FILE).unwrap(),
        };
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","File list: {:?}", resp);

        let file_size = resp.file_size;

        let cmd = file::Open {
            filename: String::try_from(CLIENT_CERT_FILE).unwrap(),
            mode: 0,
        };
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        let file_id = resp.handle;
        log::debug!(target: "quectel","File open OK");

        let cmd = file::Seek {
            handle: file_id,
            offset: 0,
            position: 0,
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","File seek OK");

        // Read file in chunk of max 1000 bytes.
        let mut remaining_file_size = file_size;
        while remaining_file_size > 0 {
            let chunk_size = 1000;
            let cmd = file::Read {
                handle: file_id,
                len: chunk_size,
            };

            let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
            log::debug!(target: "quectel","File read OK: {:?}", resp.bytes.len());

            remaining_file_size = remaining_file_size.saturating_sub(resp.bytes.len() as u32);
            if remaining_file_size == 1 && resp.bytes.len() < chunk_size as usize {
                log::debug!(target: "quectel","Remaining file size: {}", remaining_file_size);
                break; // we are done
            }
        }

        let cmd = file::Close { handle: file_id };

        self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","File close OK");

        // Upload a new file

        // First try delete

        log::debug!(target: "quectel","Prepare file upload");
        let cmd = file::Delete {
            filename: String::try_from("UFS:test_file.txt").unwrap(),
        };
        self.client.send(&cmd).await.ok();

        let cmd = file::UploadInit {
            filename: String::try_from("UFS:test_file.txt").unwrap(),
            file_size: TEST_FILE.len() as u32,
            timeout: 60,
        };
        log::debug!(target: "quectel","Wait for CONNECT");
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let upload_result: Upload = self
            .client
            .send_bytes(TEST_FILE.as_bytes(), 300_000)
            .await
            .map_err(Error::Client)?;
        if TEST_FILE.len() != upload_result.size as usize {
            return Err(Error::Serialize);
        }
        log::debug!(target: "quectel","File upload done");

        let cmd = file::Open {
            filename: String::try_from("UFS:test_file.txt").unwrap(),
            mode: 0,
        };
        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        let file_id = resp.handle;
        log::debug!(target: "quectel","File open OK");

        let cmd = file::Seek {
            handle: file_id,
            offset: 0,
            position: 0,
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","File seek OK");

        let cmd = file::Read {
            handle: file_id,
            len: 100,
        };

        let resp = self.client.send(&cmd).await.map_err(Error::Client)?;
        log::debug!(target: "quectel","File read OK: {:?}", resp);

        Ok(())
    }

    pub async fn connect(
        &mut self,
        _client_id: &str,
        will: Will,
    ) -> Result<(), Error<atat::Error>> {
        let cmd = packetdomain::SetPDPContextDefinition {
            cid: MQTT_CONTEXT,
            pdp_type: String::try_from("IP").unwrap(),
            apn: String::try_from(self.apn).unwrap(),
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Check CREG
        let cmd = general::CREGQuery;
        let mut retries = 90;
        while retries > 0 {
            match self.client.send(&cmd).await {
                Ok(resp) if resp.stat == 1 || resp.stat == 5 => {
                    log::debug!(target: "quectel","Network registered");
                    break;
                }
                _ => {
                    retries -= 1;
                    delay_ms(1000).await;
                }
            };
        }

        // Check CEREG
        let cmd = general::CEREGQuery;
        let mut retries = 60;
        while retries > 0 {
            match self.client.send(&cmd).await {
                Ok(resp) if resp.stat == 1 || resp.stat == 5 => {
                    log::debug!(target: "quectel","LTE registered");
                    break;
                }
                _ => {
                    retries -= 1;
                    delay_ms(1000).await;
                }
            };
        }

        // Activate context
        let cmd = tcpip::ActivateContext { cid: MQTT_CONTEXT };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let cmd = mqtt::ConfigRecvMode::new(MQTT_CONTEXT, 1, Some(1));
        self.client.send(&cmd).await.map_err(Error::Client)?;

        if self.config.use_tls {
            self.setup_tls().await?;
        }

        // Set up the will
        let cmd = mqtt::ConfigWill::new(MQTT_CONTEXT, 1, 1, 1, will.topic, will.payload);
        if let Err(err) = self.client.send(&cmd).await.map_err(Error::Client) {
            log::error!(target: "quectel","Failed to set will: {err:?}");
        }

        self.connect_mqtt(true).await
    }

    pub async fn subscribe(
        &mut self,
        topic: String<{ MAX_TOPIC_LEN }>,
    ) -> Result<(), Error<atat::Error>> {
        let msg_id = self.tx_msg_id();
        let cmd = mqtt::Subscribe {
            client_idx: MQTT_CONTEXT,
            msg_id,
            topic: topic.clone(),
            qos: 2,
        };

        self.client.send(&cmd).await.map_err(Error::Client)?;

        let result = self
            .await_acked_with_timeout(UrcVariant::MQTTSubscribe, msg_id, Error::MQTT("Subscribe"))
            .await;
        match &result {
            Ok(_) => log::debug!(target: "quectel","Subscribed to {topic}"),
            Err(err) => log::error!(target: "quectel","Failed to subscribe: {err:?}"),
        }
        result
    }

    pub async fn unsubscribe(
        &mut self,
        topic: String<{ MAX_TOPIC_LEN }>,
    ) -> Result<(), Error<atat::Error>> {
        let msg_id = self.tx_msg_id();
        let cmd = mqtt::Unsubscribe {
            client_idx: MQTT_CONTEXT,
            msg_id,
            topic: topic.clone(),
        };

        self.client.send(&cmd).await.map_err(Error::Client)?;

        let result = self
            .await_acked_with_timeout(
                UrcVariant::MQTTUnsubscribe,
                msg_id,
                Error::MQTT("Unsubscribe"),
            )
            .await;
        match &result {
            Ok(_) => log::debug!(target: "quectel","Unsubscribed to {topic}"),
            Err(err) => log::error!(target: "quectel","Failed to unsubscribe: {err:?}"),
        }
        result
    }

    pub async fn publish(
        &mut self,
        topic: String<{ MAX_TOPIC_LEN }>,
        message: &[u8],
    ) -> Result<(), Error<atat::Error>> {
        let msg_id = self.tx_msg_id();
        let cmd = mqtt::PublishPrepare {
            client_idx: MQTT_CONTEXT,
            msg_id,
            qos: 1,
            retain: 0,
            topic: topic.clone(),
            msg_len: message.len() as u16,
        };

        if message.len() > MAX_MESSAGE_LEN {
            log::error!(
                "Cannot publish {}-byte message: exceeds maximum of {}!",
                message.len(),
                MAX_MESSAGE_LEN
            );
            return Err(Error::Serialize);
        }

        self.client.send(&cmd).await.map_err(Error::Client)?;

        // Wait for prompt: handled by atat.

        // Send data directly as slice
        self.client
            .send_bytes::<NoResponse>(message, 3000)
            .await
            .map_err(Error::Client)?;

        // Wait untill mesasge is acked by broker
        let result = self
            .await_acked_with_timeout(UrcVariant::MQTTPublish, msg_id, Error::MQTT("Publish"))
            .await;
        match &result {
            Ok(_) => log::debug!(target: "quectel","Mqtt published to {topic:?}"),
            Err(err) => log::error!(target: "quectel","Failed to publish: {err:?}"),
        }
        result
    }

    async fn connect_mqtt(&mut self, clean_session: bool) -> Result<(), Error<atat::Error>> {
        self.packets_missed = false;
        self.last_rx_msg_id = 0;

        let cmd = mqtt::ConfigSession::new(MQTT_CONTEXT, clean_session);

        self.client.send(&cmd).await.map_err(Error::Client)?;

        // All config should happen before Open.
        let cmd = mqtt::Open {
            client_idx: MQTT_CONTEXT,
            host_name: String::try_from(self.config.host).unwrap(),
            port: self.config.port,
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        if let Some(Urc::MQTTOpen(resp)) = self.await_urc(UrcVariant::MQTTOpen).await {
            match resp.parse() {
                mqtt::urc::MQTTOpenResult::Success => {}
                err => {
                    log::error!(target: "quectel","Error during MQTTOpen: {err:?}");
                    return Err(Error::MQTT("Open"));
                }
            }
        } else {
            return Err(Error::TimeOut);
        }

        let cmd = mqtt::Connect {
            client_idx: MQTT_CONTEXT,
            // Client id is determined by the certificate.
            mqtt_client_id: String::try_from("placeholder").unwrap(),
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        if let Some(Urc::MQTTConnect(resp)) = self.await_urc(UrcVariant::MQTTConnect).await {
            match resp.parse() {
                mqtt::urc::MQTTConnectResult::Accepted => {
                    log::debug!(target: "quectel","Mqtt connected");
                    Ok(())
                }
                err => {
                    log::error!(target: "quectel","Failed to connect mqtt: {err:?}");
                    Err(Error::MQTT("Connect"))
                }
            }
        } else {
            Err(Error::TimeOut)
        }
    }

    async fn disconnect_mqtt(&mut self) -> Result<(), Error<atat::Error>> {
        let cmd = mqtt::Disconnect {
            client_idx: MQTT_CONTEXT,
        };

        // Try to send MQTT disconnect. Note that even if this fails, we still continue to try and close the TCP connection
        match self.client.send(&cmd).await {
            Ok(_) => {
                if let Some(Urc::MQTTDisconnect(resp)) =
                    self.await_urc(UrcVariant::MQTTDisconnect).await
                {
                    match resp.parse() {
                        mqtt::urc::MQTTResult::Success => {}
                        mqtt::urc::MQTTResult::Failed => {
                            log::error!(target: "quectel","Mqtt disconnect failed (urc)")
                        }
                    }
                } else {
                    log::error!(target: "quectel","Mqtt disconnect failed (urc timeout)")
                }
            }
            Err(err) => log::error!(target: "quectel","Error mqtt disconnect: {err:?}"),
        };

        // Try to close MQTT TCP connection, just in case (after MQTT disconnect the connection is often already closed)

        match self
            .client
            .send(&mqtt::Close {
                client_idx: MQTT_CONTEXT,
            })
            .await
        {
            Ok(_) => {
                if let Some(Urc::MQTTClose(resp)) = self.await_urc(UrcVariant::MQTTClose).await {
                    match resp.parse() {
                        mqtt::urc::MQTTResult::Success => {}
                        mqtt::urc::MQTTResult::Failed => {
                            log::warn!(target: "quectel","MQTT close failed: unexpected result")
                        }
                    }
                } else {
                    log::warn!(target: "quectel","MQTT close failed: timeout")
                }
            }
            Err(_) => {
                log::warn!(target: "quectel","MQTT close failed")
            }
        }

        Ok(())
    }

    /// Try to disconnect from the MQTT broker & close the PDP context
    ///
    /// If this fails, the function returns an error. Note that such error
    /// does not imply that the network is still connected.
    pub async fn disconnect(&mut self) -> Result<(), Error<atat::Error>> {
        self.disconnect_mqtt().await.ok();

        // Try to deactivate the PDP context
        self.client
            .send(&tcpip::DeactivateContext { cid: MQTT_CONTEXT })
            .await
            .map(|_| ())
            .map_err(|err| {
                log::error!(target: "quectel","Failed to deactivate context: {err:?}");
                Error::MQTT("Deactivate Context")
            })
    }

    /// Turns off the modem
    ///
    /// Note: after this, it is best to construct a new EC25 instance before re-initializing.
    /// There may be residual state that is not properly reset such as in the QuectelBackend..
    pub async fn deinitialize(&mut self) {
        match self.client.send(&general::PowerDown::new()).await {
            Ok(_) => {
                log::debug!(target: "quectel","Send PowerDown OK");
                match self.await_urc(UrcVariant::ModemOff).await {
                    Some(Urc::ModemOff) => {
                        log::debug!(target: "quectel","Modem is off!")
                    }
                    _unexpected => {
                        log::warn!(target: "quectel","Failed to detect modem off:  {:?}", _unexpected)
                    }
                }
            }
            Err(_) => log::error!(target: "quectel","Send PowerDown Failed"),
        }

        delay_ms(1_000).await;

        // Suspend UART before modem power down
        self.uart.suspend();

        self.power.power_off();
        log::debug!(target: "quectel","Modem fully powered down!");

        // Flush queues. Note that we cant control the QuectelBackend, which might still contain half-parsed stuff...
        while let Some(_) = self.urc_subscription.try_next_message() {}
        while let Some(_) = self.unexpected_urc_queue.dequeue() {}
    }

    async fn turn_on(&mut self) {
        // Already turned on? Make sure uart is resumend and skip the rest
        if self.power.is_powered() {
            self.uart.resume();
            return;
        }
        log::debug!(target: "quectel","Turning on modem...");

        // Power-on pin sequence, including settling delays before resuming UART:
        // we want to be sure the modem is powered before we set TX high.

        // If this delay ever gives issues in the future, we could split resume into two stages:
        // resume_rx(), then enable modem, then resume_tx().
        self.power.power_on().await;
        self.uart.resume();

        // If we hit the timeout or receive unexpected URC data, the modem was probably already on?
        // This parsing could be made stricter if we don't support the devkit..
        match self.await_urc(UrcVariant::ModemReady).await {
            Some(Urc::ModemReady) => {
                log::debug!(target: "quectel","Modem is on");
            }
            _unexpected => {
                log::debug!(target: "quectel","Got {:?}, modem is probably (still?) on?", _unexpected)
            }
        }
    }

    async fn setup_tls(&mut self) -> Result<(), Error<atat::Error>> {
        // Activate context
        let cmd = packetdomain::SetPDPContextDefinition {
            cid: TLS_CONTEXT,
            pdp_type: String::try_from("IP").unwrap(),
            apn: String::try_from(self.apn).unwrap(),
        };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        let cmd = tcpip::ActivateContext { cid: TLS_CONTEXT };
        self.client.send(&cmd).await.map_err(Error::Client)?;

        // AT+QMTCFG="SSL",1,1,2
        let cmd = mqtt::ConfigSSL::new(MQTT_CONTEXT, 1, Some(TLS_CONTEXT));
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="cacert",2,"UFS:cacert.pem"
        let cmd = ssl::SSLCFGFile::cacert(TLS_CONTEXT, String::try_from(CA_FILE).unwrap());
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="clientcert",2,"UFS:client.crt"
        let cmd =
            ssl::SSLCFGFile::clientcert(TLS_CONTEXT, String::try_from(CLIENT_CERT_FILE).unwrap());
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="clientkey",2,"UFS:client.key"
        let cmd =
            ssl::SSLCFGFile::clientkey(TLS_CONTEXT, String::try_from(CLIENT_KEY_FILE).unwrap());
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="seclevel",2,2
        let cmd = ssl::SSLCFG::seclevel(TLS_CONTEXT, 2);
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="sslversion",2,3
        let cmd = ssl::SSLCFG::sslversion(TLS_CONTEXT, 3);
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="ciphersuite",2,0xFFFF
        // let cmd = ssl::SSLCFGCiphersuite::new(TLS_CONTEXT, 0xFFFF);
        let cmd = ssl::SSLCFGCiphersuiteFixed;
        self.client.send(&cmd).await.map_err(Error::Client)?;
        // AT+QSSLCFG="ignorelocaltime",2,1
        let cmd = ssl::SSLCFG::ignorelocaltime(TLS_CONTEXT, 1);
        self.client.send(&cmd).await.map_err(Error::Client)?;

        Ok(())
    }

    fn unexpected_urc(&mut self, urc: Urc) {
        if let Err(_err) = self.unexpected_urc_queue.enqueue(urc) {
            log::error!(target: "quectel","URC queue overflow");
        }
    }

    /// Await URC untill variant-specific timeout
    async fn await_urc(&mut self, variant: UrcVariant) -> Option<Urc> {
        let timeout = variant.timeout();
        self.inner_await_urc(variant).with_timeout_ms(timeout).await
    }

    /// await URC forever (no timeout)
    async fn inner_await_urc(&mut self, variant: UrcVariant) -> Urc {
        loop {
            let urc = self.urc_subscription.next_message_pure().await;

            if urc.variant() == variant {
                return urc;
            } else {
                self.unexpected_urc(urc);
            }
        }
    }

    async fn await_acked_with_timeout(
        &mut self,
        variant: UrcVariant,
        expected_msg_id: u16,
        default_error: Error<atat::Error>,
    ) -> Result<(), Error<atat::Error>> {
        // Build a future that waits for and parses the expected URC
        let fut = async {
            loop {
                let packet_result = match self.inner_await_urc(variant).await {
                    Urc::MQTTSubscribe(msg) => msg.parse_packet_result(expected_msg_id),
                    Urc::MQTTUnsubscribe(msg) => msg.parse_packet_result(expected_msg_id),
                    Urc::MQTTPublish(msg) => msg.parse_packet_result(expected_msg_id),

                    // This method only makes sense for MQTT sub/unsub/publish. Other variants do not implement MQTTAck
                    _other => break Err(Error::Client(atat::Error::Custom)),
                };
                match packet_result {
                    // Success: publish is acked (by broker for QOS>0)
                    Ok(mqtt::urc::MQTTPacketResult::Success) => {
                        break Ok(());
                    }

                    // Fail: publish definitely failed
                    Ok(_err) => {
                        break Err(default_error);
                    }

                    // Retransmission: modem is trying again. We don't have to do anything here.
                    // Retransmission URC alwasy have msg_id 0 which will mismatch (as 0 is not valid for normal use in QOS>0)
                    // (this is because msg_id is normally set by broker but on retransmission it is made up by the modem instead)
                    Err(mqtt::urc::MQTTPacketResult::Retransmission) => {
                        log::warn!(target: "quectel","Retransmission");
                        // Modem will automatically retransmit and send another URC later
                    }

                    // Mismatching msg_id: ignore. This typically happens after a retransmission where the old msg_id was acked multiple times
                    Err(unexpected) => {
                        log::debug!("Ignore {unexpected:?} (mismatching message id)")
                    }
                }
            }
        };

        // Run the future with a timeout
        match fut.with_timeout_ms(variant.timeout()).await {
            Some(res) => res,
            None => Err(Error::TimeOut),
        }
    }

    /// Read mqtt message from a slot with index `recv_id`.
    async fn read_message(&mut self, recv_id: u8) -> Result<MQTTReceive, Error<atat::Error>> {
        if recv_id == 4 {
            // We might have missed packets as slot 4 (of 0-4) is used.
            self.packets_missed = true;
            log::debug!(target: "quectel","Missed packets: slot 4");
        }
        let cmd = mqtt::Receive {
            client_idx: MQTT_CONTEXT,
            recv_id: Some(recv_id),
        };
        let res = self.client.send(&cmd).await.map_err(Error::Client)?;
        Ok(res)
    }

    async fn handle_urc(&mut self) -> Result<Event, ()> {
        let Some(urc) = self.urc.take() else {
            return Err(());
        };
        match urc {
            Urc::MQTTReceive(urc) => match self.read_message(urc.recv_id).await {
                Ok(message) => {
                    log::debug!(target: "quectel","MQTT message on: {:?}", message.topic);
                    // If last_rx_msg_id == 0 we do not check to prevent false positives.
                    if self.last_rx_msg_id > 0 && (message.msg_id > self.last_rx_msg_id + 1) {
                        log::debug!(target: "quectel",
                            "Missed packets: {} > {}",
                            message.msg_id,
                            self.last_rx_msg_id + 1
                        );
                        // This causes too many false positives.
                        // self.packets_missed = true;
                    }
                    self.last_rx_msg_id = message.msg_id;

                    return Ok(Event::ReceivedMessage(message.into()));
                }
                Err(err) => {
                    log::error!(target: "quectel","Failed to read message: {err:?}");
                    return Err(());
                }
            },
            Urc::MQTTStatus(status) => match status.parse() {
                mqtt::urc::MQTTStatusResult::ConnectionClosed => return Ok(Event::Disconnected),
                stat => {
                    log::warn!(target: "quectel","MQTT status: {stat:?}");
                    return Err(());
                }
            },
            Urc::QIURC(res) => {
                log::warn!(target: "quectel","Received unexpected tcp URC: {:?}", res);
                return Err(());
            }
            Urc::CREG(creg) => {
                log::debug!(target: "quectel","Got creg status: {creg:?}");
                return Err(());
            }
            Urc::EREG(creg) => {
                log::debug!(target: "quectel","Got ereg status: {creg:?}");
                return Err(());
            }
            Urc::MQTTOpen(_) => return Err(()),
            Urc::MQTTClose(_) => return Err(()),
            Urc::MQTTConnect(_) => return Err(()),
            Urc::MQTTDisconnect(_) => return Err(()),
            Urc::MQTTSubscribe(_) => return Err(()),
            Urc::MQTTUnsubscribe(_) => return Err(()),
            Urc::MQTTPublish(_) => return Err(()),
            Urc::ModemReady => return Err(()),
            Urc::ModemOff => return Err(()),
            Urc::HTTPGet(_) => return Err(()),
            Urc::HTTPReadFile(_) => return Err(()),
        }
    }

    /// Generate the next message id for transmitting
    fn tx_msg_id(&mut self) -> u16 {
        // Wrapping add, skipping 0 (zero is reserved in MQTT)
        let next_id = match self.next_tx_msg_id {
            u16::MAX => 1,
            msg_id => msg_id + 1,
        };
        self.next_tx_msg_id = next_id;
        next_id
    }
}

impl<R: Read + ErrorReport + Suspendable> QuectelBackend<R> {
    /// Reads and processes incoming data over uart.
    pub async fn read_incoming(&mut self) {
        loop {
            let buf = self.ingress.write_buf();
            match self.rx.read(buf).await {
                Ok(num_read) => {
                    if num_read > 0 {
                        self.ingress.advance(num_read).await;
                    }
                }
                Err(err) => {
                    log::debug!(target: "quectel","Uart read error: {err:?}");
                    if self.rx.error_occurred() {
                        ERROR_OCCURRED.store(true, Ordering::Relaxed);
                        self.ingress.clear();
                        self.rx.reset_error();
                    }
                    if self.rx.is_suspended() {
                        log::debug!(target: "quectel","modem_rx wait for resume");
                        self.rx.await_resume().await;
                        log::debug!(target: "quectel","modem_rx resumed");
                    }
                }
            }
        }
    }
}

impl<W, R, V, U> MqttClient for Quectel<W, R, V, U>
where
    W: Write,
    R: Read + ErrorReport + Suspendable,
    V: ModemVariant,
    U: Suspend + BaudRateControl,
{
    type ClientError = atat::Error;
    type PollError = ();

    /// Connect and get ready
    async fn connect(
        &mut self,
        client_id: &str,
        will: Will,
    ) -> Result<(), Error<Self::ClientError>> {
        self.initialize().await?;
        self.connect(client_id, will).await
    }

    async fn disconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        self.disconnect().await.ok();
        self.deinitialize().await;
        Ok(())
    }

    async fn reconnect(&mut self) -> Result<(), Error<Self::ClientError>> {
        self.disconnect_mqtt().await?;
        self.connect_mqtt(false).await
    }

    async fn subscribe(
        &mut self,
        topic_name: heapless::String<{ MAX_TOPIC_LEN }>,
    ) -> Result<(), Error<Self::ClientError>> {
        self.subscribe(topic_name).await
    }

    async fn unsubscribe(
        &mut self,
        topic_name: heapless::String<{ MAX_TOPIC_LEN }>,
    ) -> Result<(), Error<Self::ClientError>> {
        self.unsubscribe(topic_name).await
    }

    async fn publish(
        &mut self,
        topic_name: heapless::String<{ MAX_TOPIC_LEN }>,
        message: &[u8],
    ) -> Result<(), Error<Self::ClientError>> {
        self.publish(topic_name, message).await
    }

    async fn handle_response(&mut self) -> Result<Event, ()> {
        self.handle_urc().await
    }

    async fn await_response(&mut self, timeout_s: u32) -> Result<(), Self::PollError> {
        if ERROR_OCCURRED.load(Ordering::Relaxed) == true {
            log::error!(target: "quectel","EC25 error occurred in uart Rx");
            // Reset state after 'reporting'
            ERROR_OCCURRED.store(false, Ordering::Relaxed);
            return Err(());
        }

        if self.urc.is_some() {
            return Ok(());
        }

        // Handle any URC received during init (e.g. qos=1,2 messages)
        if let Some(urc) = self.unexpected_urc_queue.dequeue() {
            self.urc = Some(urc);
            return Ok(());
        }

        // TODO the ERROR_OCCURRED flag is not polled here.
        // This means recovery from an error can potentially be very slow (bug#210?)
        // in case the application has set a long timeout (the longest timeout we set is 1 day!)
        match self
            .urc_subscription
            .next_message_pure()
            .with_timeout_ms(timeout_s * 1000)
            .await
        {
            Some(urc) => {
                self.urc = Some(urc);
                return Ok(());
            }
            None => {
                // Timeout
                Err(())
            }
        }
    }

    /// Ask client to download a file from a specific url.
    /// Return Ok when download is done and temporarily stored for later retrieval.
    async fn download_file(&mut self, url: &str) -> Result<(), Error<Self::ClientError>> {
        // Delete file if it exists
        let _ = self.close_file().await;
        let _ = self.delete_file(RAM_TMP_FILE).await;
        self.download_file(url).await
    }

    /// Read chunks of the downloaded file. Returns None if everything is read or when there is no data.
    async fn read_file_chunk(
        &mut self,
        chunk_size: usize,
    ) -> Result<Vec<u8, MAX_FILE_CHUNK_LEN>, FileError> {
        match self.read_file_chunk(RAM_TMP_FILE, chunk_size).await {
            Ok(Some(chunk)) => Ok(chunk),
            Ok(None) => {
                // Discard the file as we finished reading it.
                let _ = self.close_file().await;
                let _ = self.delete_file(RAM_TMP_FILE).await;
                Err(FileError::EndOfFile)
            }
            Err(_) => Err(FileError::ReadError),
        }
    }

    async fn signal_quality(&mut self) -> Option<i16> {
        self.get_signal_quality().await.ok()
    }

    async fn modem_model(&mut self) -> VersionString {
        let cmd = general::GetModelId;
        let model_id = match self.client.send(&cmd).await.map_err(Error::Client) {
            Ok(v) => v.id,
            Err(e) => {
                log::error!(target: "quectel","Error getting modem model: {e:?}");
                return VersionString::new();
            }
        };

        into_version_string(model_id)
    }

    async fn modem_fw_version(&mut self) -> VersionString {
        let cmd = general::GetSoftwareVersion;
        let version = match self.client.send(&cmd).await.map_err(Error::Client) {
            Ok(v) => v.id,
            Err(e) => {
                log::error!(target: "quectel","Error getting modem fw version: {e:?}");
                return VersionString::new();
            }
        };

        into_version_string(version)
    }
}

fn into_version_string<const N: usize>(longer_string: heapless::String<N>) -> VersionString {
    let small_string: VersionString = match heapless::String::try_from(&longer_string[..]) {
        Ok(s) => s,
        Err(_) => {
            // Handle the error case appropriately
            let mut s = VersionString::new();
            s.push_str(&longer_string[..VERSION_STRING_MAX_LEN.min(longer_string.len())])
                .unwrap_or(());
            s
        }
    };
    small_string
}
