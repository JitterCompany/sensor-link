mod device;
mod net;
mod signal_gen;

use std::{
    future::Future,
    io::{stdout, Write},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use futures::{
    future::{join_all, select},
    pin_mut,
};
use sensor_link_firmware::{
    drivers::time::{self, init_timer, AdjustableTimestampSource, TimestampSource},
    logic::{
        signal::{CmdSource, Signal},
        SendChannel,
    },
    mqtt::log_publish::{self, LogPublishConfig},
    sensor_link_protocol::cmd::Cmd,
    utils::channels::make_channel,
};
use simplelog::*;

struct Timer();

static TIMER: Timer = Timer();

#[derive(Clone)]
pub struct SensorArgs {
    pub instance_no: usize,
    pub broker_host: String,
    pub broker_port: u16,
    pub sync_interval: Duration,
    pub cert_dir: Option<PathBuf>,
    pub cacert: Option<PathBuf>,
    pub use_tls: bool,
}

/// Sensor Link Mock Device
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// MQTT broker hostname.
    #[arg(long, env = "MQTT_BROKER_HOST_NAME", default_value = "localhost")]
    broker_host: String,

    /// MQTT broker port.
    #[arg(long, env = "MQTT_BROKER_PORT", default_value_t = 1883)]
    broker_port: u16,

    /// Number of sensors to spawn.
    #[arg(short, long, default_value_t = 1)]
    n: usize,

    /// Enable TLS.
    #[arg(short, long, default_value_t = false)]
    tls: bool,

    /// Path to CA certificate (required if --tls is set)
    #[arg(long)]
    cacert: Option<PathBuf>,

    /// Path to certificates for all N mock sensors.
    /// Required if TLS is enabled and N > 1.
    #[arg(short, long)]
    certs: Option<PathBuf>,

    /// Seconds between syncs. The `sync` command triggers one earlier.
    #[arg(long, default_value_t = device::DEFAULT_SYNC_INTERVAL.as_secs())]
    sync_interval: u64,

    /// Log level.
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Maximum log level published to the MQTT log topic.
    #[arg(long, default_value = "warn")]
    mqtt_log_level: String,

    /// Disable interactive input
    #[arg(long, default_value_t = false)]
    no_interactive: bool,
}

/// Parse a log level, falling back to [`LevelFilter::Off`].
///
/// An unknown name falls back to the quietest level rather than a default in
/// the middle: a typo in `--mqtt-log-level` then publishes nothing instead of
/// putting more on the air than was asked for.
fn level_filter(flag: &str, name: &str) -> LevelFilter {
    match name.to_lowercase().as_str() {
        "off" => LevelFilter::Off,
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        other => {
            eprintln!(
                "Warning: '{flag}' must be one of off, error, warn, info, debug, trace \
                 (got '{other}'); logging is off."
            );
            LevelFilter::Off
        }
    }
}

async fn read_line() -> String {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        line
    })
    .await
    .unwrap()
}

#[tokio::main]
async fn main() {
    sensor_link_firmware::std_monotonic_driver::start();
    let args = Args::parse();

    // Runtime validation for conditional requirement of certs
    if args.tls && args.n > 1 && args.certs.is_none() {
        eprintln!("Error: '--certs' is required when 'tls' is enabled and 'n' is greater than 1.");
        std::process::exit(1);
    }
    if args.tls && args.cacert.is_none() {
        eprintln!("Error: '--cacert' is required when --tls is set.");
        std::process::exit(1);
    }

    let log_level = level_filter("--log-level", &args.log_level);
    let mqtt_log_level = level_filter("--mqtt-log-level", &args.mqtt_log_level);

    let log_cfg = ConfigBuilder::new()
        .set_target_level(log_level)
        .set_thread_level(LevelFilter::Off)
        .set_location_level(LevelFilter::Off)
        .build();

    // The MQTT logger wraps the local one: records still reach the terminal, and
    // those passing `mqtt_log_level` are additionally queued for publication.
    // Both loggers only ever see records up to `log_level`, so that has to be
    // the more verbose of the two.
    let local_logger = SimpleLogger::new(log_level, log_cfg);
    let log_source = log_publish::init(
        LogPublishConfig {
            level: mqtt_log_level,
            max_level: log_level.max(mqtt_log_level),
            // The MQTT driver logs a line per publish; publishing those would
            // keep the device talking to itself.
            exclude_targets: &["mqtt"],
        },
        Some(Box::leak(local_logger)),
    )
    .expect("No other logger was installed");

    log::info!(target: "main", "Starting {n} mock device{s}", n = args.n, s = if args.n > 1 { "s" } else { "" });

    init_timer(&TIMER).unwrap();

    // The logger is global, so only the first instance can drain its queue.
    let mut log_source = Some(log_source);

    let mut devices = Vec::new();
    let mut signal_senders = Vec::new();
    for i in 0..args.n {
        let (tx, rx) = make_channel::<Signal>(10);

        let sensor_args = SensorArgs {
            instance_no: i,
            broker_host: args.broker_host.clone(),
            broker_port: args.broker_port,
            sync_interval: Duration::from_secs(args.sync_interval),
            cacert: args.cacert.clone(),
            cert_dir: args.certs.clone(),
            use_tls: args.tls,
        };

        let log_source = log_source.take();
        signal_senders.push(tx.clone());
        let handle =
            tokio::spawn(
                async move { device::run_instance(sensor_args, log_source, tx, rx).await },
            );
        devices.push(handle);
    }

    // Skip interactive input if no_interactive is set
    if !args.no_interactive {
        loop {
            println!();
            print!("> ");
            stdout().flush().unwrap();
            let line = read_line().await;
            let parts: Vec<&str> = line.split_whitespace().collect();
            match parts.as_slice() {
                ["start"] => {
                    for tx in signal_senders.iter_mut() {
                        tx.try_send(Signal::Command(Cmd::Start, CmdSource::RTT))
                            .ok();
                    }
                }
                ["stop"] => {
                    for tx in signal_senders.iter_mut() {
                        tx.try_send(Signal::Command(Cmd::Stop, CmdSource::RTT)).ok();
                    }
                }
                ["sync"] => {
                    for tx in signal_senders.iter_mut() {
                        tx.try_send(Signal::UrgentEvent).ok();
                    }
                }
                ["q"] => {
                    // The device tasks run until the process ends, so joining
                    // them here would hang instead of exiting.
                    println!("Exit now");
                    return;
                }
                other => {
                    println!("Error, didn't understand '{other:?}'");
                }
            }
        }
    }

    let results = join_all(devices).await;
    println!("Results: {results:?}");
}

impl TimestampSource for Timer {
    fn timestamp_us(&self) -> Result<i64, time::Error> {
        let start = SystemTime::now();
        let since_the_epoch = start
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        Ok(since_the_epoch.as_micros() as i64)
    }
}

impl AdjustableTimestampSource for Timer {
    fn adjust_us(&self, _target_time_us: i64, _offset_limit: u32) -> Result<i64, time::Error> {
        // Ignore for Mock device, we are not going to change system time.
        Ok(0)
    }
}

async fn with_timeout<F: Future>(
    m: F,
    dur: Duration,
) -> Option<<F as std::future::Future>::Output> {
    let n = tokio::time::sleep(dur);
    pin_mut!(m, n);

    match select(m, n).await {
        futures::future::Either::Left((l, _)) => Some(l),
        futures::future::Either::Right((_, _)) => None,
    }
}
