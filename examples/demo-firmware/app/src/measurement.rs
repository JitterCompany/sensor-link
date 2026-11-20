//! Measurement task.
//!
//! Samples the four demo channels in fixed 250 ms slots: the board temperature
//! through the internal ADC, and the charger's input bus voltage and current
//! and its system rail voltage through its own ADC over I2C. `Samples` batches
//! are sent over the session channel.

use demo_firmware_core::{
    demo_protocol::{
        event::Event,
        status::{ChannelError, ChannelStatus},
        DemoChannel,
    },
    drivers::{bq25672::BatteryCharger as _, time},
    futures::{
        future::{select, Either},
        pin_mut,
    },
    log,
    logic::{
        demo_processing::{
            input::{Samples, DEMO_N_CH, N_SAMPLES},
            INPUT_FS, TASK_INIT_TIMEOUT_MS as PROCESSING_START_TIMEOUT_MS,
        },
        signal, DebouncedChannelStatus, LatestValueReceiveChannel, SendChannel,
    },
    monotonic_time::{self, FutureTimeout as _, MonotonicInstant},
    sync::session_channel::traits::SessionSender as _,
    traits::Arbiter as _,
};

use crate::{BoardAdc, CancelRx, ChargerArbiter, EventTx, MeasurementsTx, SignalTx};

/// FS of channel samples
pub const FS: u32 = 4;

/// Duration of a single sample slot, in microseconds.
const SLOT_PERIOD_US: u64 = const {
    assert!(1_000_000 % FS as u64 == 0);
    1_000_000 / FS as u64
};

// The sample stream is produced at the rate the processing expects
const _: () = assert!(FS == INPUT_FS.value() as u32);

/// The hardware the task samples
pub struct Sensors {
    pub adc: &'static BoardAdc,
    pub charger: &'static ChargerArbiter,
}

pub async fn measurement_task(
    cancel_token: &mut CancelRx,
    data_channel: &mut MeasurementsTx,
    sensors: &Sensors,
    signal_sender: &mut SignalTx,
    event_sender: &mut EventTx,
) {
    log::info!(target: "Measurement", "Trying to start..");
    // try to wait other end of data channel is ready
    // (timeout in case other end already finished)
    let data_ch = data_channel
        .wait_started()
        .with_timeout_ms(PROCESSING_START_TIMEOUT_MS)
        .await;

    let result: Result<(), Event> = match data_ch {
        Some(Ok(session_id)) => {
            let mut task = Task::start();
            log::info!(target: "Measurement", "Started session {}", session_id);
            signal_sender
                .send(signal::Signal::Common(signal::CommonSignal::SensorStarted))
                .await
                .ok();

            loop {
                let samples_fut = task.do_measurements(sensors, signal_sender);
                let cancel_fut = cancel_token.recv();

                let samples = {
                    pin_mut!(samples_fut, cancel_fut);
                    match select(samples_fut, cancel_fut).await {
                        Either::Left((samples, _)) => samples,
                        Either::Right((_, _)) => {
                            break Ok(());
                        }
                    }
                };
                // Send data, detecting full channel as a warning Event
                match data_channel.try_send(samples) {
                    Ok(_) => {}
                    Err(data) => {
                        log::warn!(target: "measurement", "Data channel full");
                        event_sender
                            .send(Event::processing_warning("Data channel full"))
                            .await
                            .ok();
                        data_channel.send(data).await.ok();
                    }
                }
            }
        }
        _ => {
            log::error!(target: "Measurement", "Data channel error");
            Err(Event::sensor_error("Channel"))
        }
    };

    log::info!(target: "Measurement", "Stop");

    // notify processing of the end of stream & wait untill it has finished
    data_channel.finish_and_wait().await.ok();

    // report that the task is stopping
    if let Err(error_event) = result {
        event_sender.send(error_event).await.ok();
        signal_sender
            .send(signal::Signal::Common(signal::CommonSignal::SensorFailed))
            .await
            .ok();
    } else {
        signal_sender
            .send(signal::Signal::Common(signal::CommonSignal::SensorStopped))
            .await
            .ok();
    }
}

struct Task {
    ch_status: [DebouncedChannelStatus; DEMO_N_CH],

    /// Deadline of the next sample slot: conversions are started on demand,
    /// so the task paces itself explicitly.
    next_slot: MonotonicInstant,
}

/// The channels the charger measures, in [`DemoChannel`] order
const CHARGER_CHANNELS: [DemoChannel; 3] =
    [DemoChannel::Vbus, DemoChannel::Ibus, DemoChannel::Vsys];

impl Task {
    fn start() -> Self {
        Self {
            next_slot: monotonic_time::now(),
            ch_status: [const { DebouncedChannelStatus::new(FS as u8) }; DEMO_N_CH],
        }
    }

    /// Record one channel's outcome for this slot. Returns whether its
    /// debounced status changed.
    fn record(
        &mut self,
        data: &mut Samples,
        ch: DemoChannel,
        slot: usize,
        value: Option<f32>,
    ) -> bool {
        let status = match value {
            Some(value) => {
                if let Some(samples) = data.ch[ch.index()].as_mut() {
                    samples[slot] = value;
                }
                ChannelStatus::Ok(ch.sensor())
            }
            None => {
                data.ch[ch.index()] = None;
                ChannelStatus::Error(ChannelError::Faulty)
            }
        };
        self.ch_status[ch.index()].update(status)
    }

    /// Perform a measurement cycle.
    ///
    /// Produces a batch of samples for each channel, updating and reporting
    /// channel status. A failing sensor only fails its own channels.
    async fn do_measurements(
        &mut self,
        sensors: &Sensors,
        signal_sender: &mut SignalTx,
    ) -> Samples {
        // Wait for the first slot of this batch before timestamping it.
        self.next_slot.to_future().await;

        let mut data = Samples {
            t: time::timestamp_or_default_us(),
            ch: [Some([0.0; N_SAMPLES]); DEMO_N_CH],
        };

        let mut status_changed = false;
        for slot in 0..N_SAMPLES {
            if slot > 0 {
                self.next_slot.to_future().await;
            }
            self.next_slot = self.next_slot.add_micros(SLOT_PERIOD_US);

            let temperature = match sensors.adc.access().await.measure().await {
                Ok(m) => Some(m.board_temp_millicelsius as f32 / 1000.0),
                Err(err) => {
                    log::warn!(target: "Measurement", "Board temperature failed: {err:?}");
                    None
                }
            };
            status_changed |=
                self.record(&mut data, DemoChannel::BoardTemperature, slot, temperature);

            let power = match sensors.charger.access().await.measure_power().await {
                Ok(m) => Some([
                    f32::from(m.adapter_mv) / 1000.0,
                    f32::from(m.current_bus_ma) / 1000.0,
                    f32::from(m.system_mv) / 1000.0,
                ]),
                Err(err) => {
                    log::warn!(target: "Measurement", "Charger ADC failed: {err:?}");
                    None
                }
            };
            for (i, ch) in CHARGER_CHANNELS.into_iter().enumerate() {
                status_changed |= self.record(&mut data, ch, slot, power.map(|p| p[i]));
            }
        }

        // send status signal if anything changed.
        if status_changed {
            signal_sender
                .send(signal::Signal::ChannelStatus(signal::AllChannelStatus {
                    sensors: self.ch_status.each_ref().map(|ch| ch.status()),
                }))
                .await
                .ok();
        }

        data
    }
}
