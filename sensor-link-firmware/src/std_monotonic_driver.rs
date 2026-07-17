use critical_section as _;

use crate::{
    drivers::timer_queue,
    logic::{ReceiveChannel, SendChannel},
    sync::OnceLock,
    utils::{
        channels::{make_channel, Sender},
        select::{select2, Select2},
    },
};

// Dummy struct to pass to the init_montonic driver
#[derive(Debug)]
pub struct Mono {}

// Struct with the actual state (maybe monotonic driver can be improved to accept this directly?)
pub struct MonoState {
    t0: std::time::Instant,
    to_interrupt: Sender<TriggerInterrupt>,
}

#[derive(Debug, Clone)]
enum TriggerInterrupt {
    Now,
    TimeExceeds(timer_queue::Ticks),
}

static STATE: OnceLock<MonoState> = OnceLock::new();

impl timer_queue::MonotonicDriver for Mono {
    fn now(&self) -> timer_queue::Ticks {
        match STATE.get() {
            Some(state) => {
                let since_t0 = state.t0.elapsed();
                let now = since_t0.as_micros() as timer_queue::Ticks;
                log::debug!(target: "std_mono_driver", "now: {now}");
                now
            }
            None => {
                log::warn!(target: "std_mono_driver", "now: driver not initialized! Assuming now==0");
                0
            }
        }
    }

    fn set_compare(&self, instant: timer_queue::Ticks) {
        STATE
            .get()
            .unwrap()
            .to_interrupt
            .clone()
            .try_send(TriggerInterrupt::TimeExceeds(instant))
            .unwrap();
    }

    fn clear_compare_flag(&self) {
        // (NOP)
    }

    fn pend_interrupt(&self) {
        STATE
            .get()
            .unwrap()
            .to_interrupt
            .clone()
            .try_send(TriggerInterrupt::Now)
            .unwrap();
    }
}

pub fn start() {
    let (tx, mut rx) = make_channel::<TriggerInterrupt>(100);
    //let _ = STATE.get();

    let state = MonoState {
        t0: std::time::Instant::now(),
        to_interrupt: tx,
    };

    // Only happens once: successive attempts fail
    if let Ok(_) = STATE.get_or_try_init(|| state) {
        // Init monotonic timer queue
        crate::init_monotonic!(Mono, Mono {});

        // Spawn the task simulating a timer peripheral
        tokio::spawn(async move {
            // start at ~infinite untill a new duration is received
            let mut duration = std::time::Duration::MAX;

            loop {
                let sleep_fut = tokio::time::sleep(duration);

                match select2(sleep_fut, rx.recv()).await {
                    Select2::A(()) => {
                        log::debug!(target: "std_mono_driver", "interrupt! (sleep duration over)");
                        duration = std::time::Duration::MAX;
                        unsafe { timer_queue::interrupt() }
                    }
                    Select2::B(cmd) => {
                        match cmd {
                            Ok(TriggerInterrupt::Now) => {
                                log::debug!(target: "std_mono_driver", "interrupt! (was pending)");
                                unsafe { timer_queue::interrupt() }
                            }
                            Ok(TriggerInterrupt::TimeExceeds(ticks)) => {
                                let t0 = STATE.get().unwrap().t0;

                                match std::time::Duration::from_micros(ticks)
                                    .checked_sub(t0.elapsed())
                                {
                                    // time in the future: set duration to sleep
                                    Some(new_duration) => {
                                        log::debug!(target: "std_mono_driver", "await t={ticks}: sleeping for {new_duration:?}");
                                        duration = new_duration
                                    }
                                    // time in the past: trigger now
                                    None => {
                                        log::debug!(target: "std_mono_driver", "interrupt! (scheduled instant is in the future)");
                                        duration = std::time::Duration::MAX;
                                        unsafe { timer_queue::interrupt() }
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        });
    }
}
