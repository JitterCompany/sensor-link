//! Publishing the device's own log records to a dedicated MQTT topic.
//!
//! Enabled by the `mqtt-log` cargo feature. Because this crate is a library,
//! the decision to publish logs (and how verbosely) belongs to the application:
//! it enables the feature, picks the levels via [`LogPublishConfig`] and hands
//! the resulting [`LogPublisher`] to [`dispatch_task`] as its `log_in`.
//!
//! [`MqttLogger`] is a [`log::Log`] implementation that wraps the application's
//! existing (local) logger: records still reach that logger, and those passing
//! the configured [`LogPublishConfig::level`] are additionally queued as
//! [`LogMessage`]s. From there the ordinary dispatch pipeline carries them:
//! persisted to the log stream, then uploaded on
//! [`TopicFromDevice::Log`](sensor_link_protocol::TopicFromDevice::Log) at a
//! lower priority than events and sensor data, so a device that loses its
//! connection still reports what happened once it reconnects.
//!
//! Queuing into that pipeline is lock-free and lossy by design: the logger never
//! blocks, never allocates and never logs, so it is safe to call from any
//! context (including interrupts). When the queue is full, records are dropped
//! and counted in [`LogPublisher::dropped`].
//!
//! [`dispatch_task`]: crate::logic::dispatch::dispatch_task
//!
//! # Feedback loop
//!
//! A driver logs the outcome of every message it publishes, so publishing such
//! a record would emit another one, forever. Records under
//! [`PUBLISH_LOG_TARGET`](super::PUBLISH_LOG_TARGET) are therefore never
//! published. Records that merely accompany network *activity* are fine: they
//! do not recur per published message. Beyond that, keep
//! [`LogPublishConfig::level`] low (the default is [`LevelFilter::Warn`]) and
//! silence noisy targets (the modem driver in particular) via
//! [`LogPublishConfig::exclude_targets`].

use core::sync::atomic::{AtomicU32, Ordering};

use log::{LevelFilter, Log, Metadata, Record, SetLoggerError};
use rtic_sync::channel::{self, ReceiveError};
use sensor_link_protocol::device_log::LogMessage;
use static_cell::StaticCell;

use crate::{
    drivers::time::timestamp_or_default_us, logic::ReceiveChannel, mqtt::PUBLISH_LOG_TARGET,
};

/// Number of log records buffered between the logger and the task publishing them.
///
/// Records logged while the queue is full are dropped, so this trades RAM for
/// how long a burst the publisher may lag behind.
pub const LOG_QUEUE_LEN: usize = 16;

type LogChannel = channel::Channel<LogMessage, LOG_QUEUE_LEN>;
type LogSender = channel::Sender<'static, LogMessage, LOG_QUEUE_LEN>;
type LogReceiver = channel::Receiver<'static, LogMessage, LOG_QUEUE_LEN>;

/// Application-provided settings for publishing logs over MQTT.
pub struct LogPublishConfig {
    /// Maximum severity published to the MQTT log topic.
    ///
    /// Records below this level still reach the wrapped local logger.
    pub level: LevelFilter,

    /// Global `log` max level, i.e. the most verbose level any logger sees.
    ///
    /// Levels above this are filtered out by the `log` macros themselves and
    /// never reach either logger, so this must be at least as verbose as
    /// [`level`](Self::level) for publishing to work.
    pub max_level: LevelFilter,

    /// Additional log targets never published, matched as a prefix of the
    /// record's target.
    ///
    /// Use this to keep noisy layers (the modem driver in particular) out of the
    /// published stream. [`PUBLISH_LOG_TARGET`] is always excluded and does not
    /// need to be listed here.
    ///
    /// `'static` because the config is owned by the global logger, which `log`
    /// requires to be `'static` itself.
    pub exclude_targets: &'static [&'static str],
}

impl Default for LogPublishConfig {
    fn default() -> Self {
        Self {
            level: LevelFilter::Warn,
            max_level: LevelFilter::Info,
            exclude_targets: &[],
        }
    }
}

/// [`log::Log`] implementation that queues records for publication over MQTT.
pub struct MqttLogger {
    config: LogPublishConfig,
    inner: Option<&'static dyn Log>,
    tx: LogSender,
    dropped: AtomicU32,
}

impl MqttLogger {
    fn should_publish(&self, metadata: &Metadata) -> bool {
        should_publish(&self.config, metadata)
    }
}

/// Whether a record is published, i.e. is not about publishing a message (which
/// would recur forever) and passes the configured level and excluded targets.
fn should_publish(config: &LogPublishConfig, metadata: &Metadata) -> bool {
    metadata.target() != PUBLISH_LOG_TARGET
        && metadata.level() <= config.level
        && !config
            .exclude_targets
            .iter()
            .any(|excluded| metadata.target().starts_with(excluded))
}

impl Log for MqttLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.should_publish(metadata) || self.inner.is_some_and(|inner| inner.enabled(metadata))
    }

    fn log(&self, record: &Record) {
        if let Some(inner) = self.inner {
            inner.log(record);
        }

        if !self.should_publish(record.metadata()) {
            return;
        }

        let message = LogMessage::from_args(
            record.level().into(),
            record.target(),
            *record.args(),
            timestamp_or_default_us() / 1000,
        );

        // Lossy on purpose: dropping a log line is preferable to blocking
        // (or panicking in) whatever task happened to log it.
        if self.tx.clone().try_send(message).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn flush(&self) {
        if let Some(inner) = self.inner {
            inner.flush();
        }
    }
}

/// Receiving end of the log queue: the records waiting to be dispatched.
///
/// This is the log source [`dispatch_task`](crate::logic::dispatch::dispatch_task)
/// takes as its `log_in`, via the [`ReceiveChannel`] impl below.
pub struct LogPublisher {
    rx: LogReceiver,
    logger: &'static MqttLogger,
}

impl LogPublisher {
    /// Number of log records dropped so far because the queue was full.
    pub fn dropped(&self) -> u32 {
        self.logger.dropped.load(Ordering::Relaxed)
    }
}

impl ReceiveChannel<LogMessage> for LogPublisher {
    type Error = ReceiveError;

    /// Await the next log record to dispatch.
    ///
    /// Returns `Err` only if the logger is gone, which cannot happen for a
    /// logger installed by [`init`] (it lives for the rest of the program).
    async fn recv(&mut self) -> Result<LogMessage, Self::Error> {
        self.rx.recv().await
    }

    fn try_recv(&mut self) -> Result<LogMessage, Self::Error> {
        self.rx.try_recv()
    }
}

static CHANNEL: StaticCell<LogChannel> = StaticCell::new();
static LOGGER: StaticCell<MqttLogger> = StaticCell::new();

/// Install the MQTT logger as the global logger, wrapping `inner` (the
/// application's local logger, e.g. an RTT one) if there is one.
///
/// Returns the [`LogPublisher`] the application drains to publish records on
/// [`TopicFromDevice::Log`].
///
/// Must be called at most once, and only if no other logger was installed:
/// `log` allows setting the global logger a single time. A second call returns
/// `Err` (and panics if the first call succeeded, as the statics backing the
/// logger are already taken).
pub fn init(
    config: LogPublishConfig,
    inner: Option<&'static dyn Log>,
) -> Result<LogPublisher, SetLoggerError> {
    let max_level = config.max_level;
    let (tx, rx) = CHANNEL.init(LogChannel::new()).split();

    let logger = LOGGER.init(MqttLogger {
        config,
        inner,
        tx,
        dropped: AtomicU32::new(0),
    });

    log::set_logger(logger)?;
    log::set_max_level(max_level);

    Ok(LogPublisher { rx, logger })
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::{Level, Metadata};

    fn metadata<'a>(level: Level, target: &'a str) -> Metadata<'a> {
        Metadata::builder().level(level).target(target).build()
    }

    #[test]
    fn test_level_filter() {
        let config = LogPublishConfig {
            level: LevelFilter::Warn,
            ..Default::default()
        };

        assert!(should_publish(&config, &metadata(Level::Error, "App")));
        assert!(should_publish(&config, &metadata(Level::Warn, "App")));
        assert!(!should_publish(&config, &metadata(Level::Info, "App")));
    }

    #[test]
    fn test_level_filter_off() {
        let config = LogPublishConfig {
            level: LevelFilter::Off,
            ..Default::default()
        };

        assert!(!should_publish(&config, &metadata(Level::Error, "App")));
    }

    #[test]
    fn test_excluded_targets() {
        let config = LogPublishConfig {
            level: LevelFilter::Trace,
            exclude_targets: &["Modem", "Flash"],
            ..Default::default()
        };

        assert!(should_publish(&config, &metadata(Level::Error, "App")));
        assert!(!should_publish(&config, &metadata(Level::Error, "Modem")));
        // Targets are matched as a prefix, so submodules are excluded too.
        assert!(!should_publish(
            &config,
            &metadata(Level::Error, "Flash::block_layer")
        ));
    }

    /// Records about publishing a message, which would publish themselves
    /// forever, are never published however permissive the configuration is.
    #[test]
    fn test_publish_target_always_excluded() {
        let config = LogPublishConfig {
            level: LevelFilter::Trace,
            exclude_targets: &[],
            ..Default::default()
        };

        assert!(!should_publish(
            &config,
            &metadata(Level::Error, PUBLISH_LOG_TARGET)
        ));
        // Other network records do not recur per published message, so they
        // are published as usual.
        assert!(should_publish(&config, &metadata(Level::Error, "Network")));
    }
}
