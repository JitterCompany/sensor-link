//! Simulated demo devices: N instances of the demo firmware logic, each with
//! its own MQTT connection and simulated measurements.

mod device;
mod net;
mod signal_gen;

use std::{
    future::Future,
    io::{stdout, Write},
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use demo_firmware_core::logic::signal::{CmdSource, CommonSignal, Signal};
use futures::{
    future::{join_all, select},
    pin_mut,
};
use sensor_link_firmware::{
    drivers::time::{self, init_timer, AdjustableTimestampSource, TimestampSource},
    logic::SendChannel,
    sensor_link_protocol::cmd::Cmd,
    utils::channels::make_channel,
};
use simplelog::*;

struct Timer();

use clap::Parser;

#[derive(Clone)]
pub struct SensorArgs {
    pub instance_no: usize,
    pub host: String,
    pub port: u16,
    pub cert_dir: Option<PathBuf>,
    pub cacert: Option<PathBuf>,
    pub use_tls: bool,
}

/// Demo Mock Device
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Number of devices to spawn, named DEMO-0001, DEMO-0002, ...
    #[arg(short, long, default_value_t = 1)]
    n: usize,

    /// MQTT broker host
    #[arg(long, default_value = "localhost")]
    host: String,

    /// MQTT broker port
    #[arg(long, default_value_t = 8883)]
    port: u16,

    /// Enable TLS.
    #[arg(short, long, default_value_t = false)]
    tls: bool,

    /// Path to CA certificate (required if --tls is set)
    #[arg(long)]
    cacert: Option<PathBuf>,

    /// Directory with a `<id>.cert` and `<id>.key` per device, as the demo
    /// server's scripts/gen-dev-certs.sh writes them. Required with --tls.
    #[arg(short, long)]
    certs: Option<PathBuf>,

    /// Log level.
    #[arg(short, long, default_value = "info")]
    log_level: String,

    /// Disable interactive input
    #[arg(long, default_value_t = false)]
    no_interactive: bool,
}

/// `None` at end of input, e.g. when stdin is not a terminal
async fn read_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    })
    .await
    .unwrap()
}

#[tokio::main]
async fn main() {
    demo_firmware_core::std_monotonic_driver::start();
    let args = Args::parse();

    if args.tls && (args.certs.is_none() || args.cacert.is_none()) {
        eprintln!("Error: '--certs' and '--cacert' are required when --tls is set.");
        std::process::exit(1);
    }
    if args.tls {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install the TLS crypto provider");
    }

    let log_level = match args.log_level.to_lowercase().as_str() {
        "info" => LevelFilter::Info,
        "warn" => LevelFilter::Warn,
        "error" => LevelFilter::Error,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Debug,
    };

    let log_cfg = ConfigBuilder::new()
        .set_target_level(log_level)
        .set_thread_level(LevelFilter::Off)
        .set_location_level(LevelFilter::Off)
        .build();
    let _ = SimpleLogger::init(log_level, log_cfg);

    log::info!(target: "main", "Starting {n} mock device{s}", n = args.n, s = if args.n > 1 { "s" } else { "" });

    init_timer(&Timer()).unwrap();

    let mut devices = Vec::new();
    let mut signal_senders = Vec::new();
    for i in 0..args.n {
        let (tx, rx) = make_channel::<Signal>(10);

        let sensor_args = SensorArgs {
            instance_no: i,
            host: args.host.clone(),
            port: args.port,
            cacert: args.cacert.clone(),
            cert_dir: args.certs.clone(),
            use_tls: args.tls,
        };

        signal_senders.push(tx.clone());
        let handle =
            tokio::spawn(async move { device::mock::run_instance(sensor_args, tx, rx).await });
        devices.push(handle);
    }

    // Skip interactive input if no_interactive is set
    if !args.no_interactive {
        loop {
            println!();
            print!("> ");
            stdout().flush().unwrap();
            let Some(line) = read_line().await else {
                println!("End of input, the devices keep running");
                break;
            };
            let parts: Vec<&str> = line.split_whitespace().collect();
            let signal: fn() -> Signal = match parts.as_slice() {
                ["start"] => || Signal::Common(CommonSignal::Command(Cmd::Start, CmdSource::RTT)),
                ["stop"] => || Signal::Common(CommonSignal::Command(Cmd::Stop, CmdSource::RTT)),
                ["sync"] => || Signal::Common(CommonSignal::UrgentEvent),
                // The button: forces a sync and sends the button_pressed event
                ["button"] => || Signal::Common(CommonSignal::ButtonPressed),
                ["q"] => {
                    println!("Exit now");
                    break;
                }
                other => {
                    println!("Error, didn't understand '{other:?}'");
                    continue;
                }
            };
            for tx in signal_senders.iter_mut() {
                tx.try_send(signal()).ok();
            }
        }
    }

    let results = join_all(devices).await;
    println!("Results: {:?}", results);
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
        // Ignore for Mock sensor, we are not going to change system time.
        Ok(0)
    }
}

async fn with_timeout<F: Future>(
    m: F,
    dur: Duration,
) -> Option<<F as std::future::Future>::Output> {
    let n = tokio::time::sleep(dur);
    pin_mut!(m, n);

    let t = select(m, n).await;
    match t {
        futures::future::Either::Left((l, _)) => Some(l),
        futures::future::Either::Right((_, _)) => None,
    }
}
