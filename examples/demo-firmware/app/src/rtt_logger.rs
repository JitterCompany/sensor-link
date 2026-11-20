//! Logging over RTT: a lock-free queue logger, drained to RTT by idle.

use core::fmt::Write;

use demo_firmware_core::{
    heapless,
    log::{self, Metadata, Record},
    static_cell::StaticCell,
};
use rtic_sync::channel;

const CHUNK_LEN: usize = 60;
pub type LogLine = heapless::Vec<u8, CHUNK_LEN>;
const N_LOG_LINE: usize = 50;
pub type LogReader = channel::Receiver<'static, LogLine, N_LOG_LINE>;
pub type LogWriter = channel::Sender<'static, LogLine, N_LOG_LINE>;

/// Lockless Logger based on queues.
pub struct QLogger {
    tx: LogWriter,
}

static CH: StaticCell<channel::Channel<LogLine, N_LOG_LINE>> = StaticCell::new();

static QLOGGER: StaticCell<QLogger> = StaticCell::new();

pub const RTT_INPUT_SIZE: usize = 16;

pub fn init_queue_logger(level: log::LevelFilter) -> LogReader {
    let (tx, rx) = CH.init(channel::Channel::new()).split();

    let logger = QLOGGER.init(QLogger { tx });
    log::set_logger(logger)
        .map(|()| log::set_max_level(level))
        .unwrap();

    rx
}

impl log::Log for QLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let mut s = heapless::String::<256>::new();
            let mut tx = self.tx.clone();
            if writeln!(
                s,
                "[{}:{}] - {}",
                record.target(),
                record.level(),
                record.args()
            )
            .is_ok()
            {
                for chunk in s.as_bytes().chunks(CHUNK_LEN) {
                    if let Ok(vec) = chunk.try_into() {
                        if tx.try_send(vec).is_err() {
                            break;
                        }
                    }
                }
            } else {
                let mut s = heapless::String::<CHUNK_LEN>::new();
                let _ = writeln!(s, "[DISCARDED]");
                if let Ok(vec) = s.as_bytes().try_into() {
                    let _ = tx.try_send(vec);
                }
            }
        }
    }

    fn flush(&self) {}
}
