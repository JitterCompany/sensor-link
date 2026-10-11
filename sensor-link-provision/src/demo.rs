//! `--demo`: a stand-in for the worker that needs no probe, YubiKey or
//! firmware zip, so the session screen and its dialogs can be tried on any
//! machine. A UID containing FAIL, NOBOOT or NOMQTT picks a failure path.

use std::{
    sync::mpsc::{Receiver, Sender},
    thread::sleep,
    time::Duration,
};

use eframe::egui;

use crate::worker::{
    Checks, Command, Event, Outcome, STEP_MQTT, STEP_RTT, STEPS, SessionInfo, StepState,
};

const MQTT_TIMEOUT_S: u64 = 5;
const MARKER: &str = sensor_link_firmware::logic::network::CONNECTED_MARKER;

pub fn spawn(commands: Receiver<Command>, events: Sender<Event>, ctx: egui::Context) {
    std::thread::Builder::new()
        .name("provision-demo".into())
        .spawn(move || {
            Demo {
                commands,
                events,
                ctx,
            }
            .run()
        })
        .expect("spawning demo thread");
}

struct Demo {
    commands: Receiver<Command>,
    events: Sender<Event>,
    ctx: egui::Context,
}

/// The operator chose Skip after a failed step.
struct Skipped;

impl Demo {
    fn send(&self, ev: Event) {
        let _ = self.events.send(ev);
        self.ctx.request_repaint();
    }

    fn step_state(&self, index: usize, state: StepState) {
        self.send(Event::Step { index, state });
    }

    fn run(self) {
        self.send(Event::SessionReady(Box::new(session_info())));
        while let Ok(cmd) = self.commands.recv() {
            match cmd {
                Command::ListProbes => self.send(Event::Probes(Vec::new())),
                Command::Provision {
                    uid,
                    reprovision,
                    checks,
                    ..
                } => self.provision(&uid, reprovision, checks),
                Command::StartSession(_) | Command::Retry | Command::Skip => {}
            }
        }
    }

    fn provision(&self, uid: &str, reprovision: bool, checks: Checks) {
        for index in 0..STEPS.len() {
            self.step_state(index, StepState::Pending);
        }
        let upper = uid.to_uppercase();
        let outcome = (0..STEP_RTT)
            .try_for_each(|index| self.step(index, index == 2 && upper.contains("FAIL")))
            .map(|()| self.verify(uid, &upper, checks));
        let (outcome, rtt_log, error) = outcome.unwrap_or_else(|Skipped| {
            (
                Outcome::Fail,
                String::new(),
                Some("skipped by operator".into()),
            )
        });
        self.send(Event::DeviceFinished {
            uid: uid.to_owned(),
            outcome,
            reprovision,
            rtt_log,
            error,
        });
    }

    /// A provisioning step; `fail` makes the first attempt fail so the
    /// Retry / Skip dialog shows. A retry then succeeds.
    fn step(&self, index: usize, mut fail: bool) -> Result<(), Skipped> {
        loop {
            self.step_state(index, StepState::Running);
            for i in 1..=10 {
                sleep(Duration::from_millis(40));
                if (3..=5).contains(&index) {
                    self.send(Event::StepProgress {
                        index,
                        fraction: i as f32 / 10.0,
                    });
                }
            }
            if !fail {
                self.step_state(index, StepState::Done);
                return Ok(());
            }
            fail = false;
            self.step_state(
                index,
                StepState::Failed("demo: probe not responding".into()),
            );
            loop {
                match self.commands.recv() {
                    Ok(Command::Retry) => break,
                    Ok(Command::Skip) | Err(_) => return Err(Skipped),
                    Ok(_) => {}
                }
            }
        }
    }

    fn verify(&self, uid: &str, upper: &str, checks: Checks) -> (Outcome, String, Option<String>) {
        let mut log = String::from("stale output from the previous boot\n");
        let mut line = |text: &str, delay_ms: u64| {
            sleep(Duration::from_millis(delay_ms));
            log.push_str(text);
            log.push('\n');
            self.send(Event::Rtt(log.clone()));
        };

        self.step_state(STEP_RTT, StepState::Running);
        line("# Starting demo firmware", 300);
        if upper.contains("NOBOOT") {
            line("panic: demo boot failure", 500);
            let msg = "UID not seen in the boot log within 10 s".to_owned();
            self.step_state(STEP_RTT, StepState::Failed(msg.clone()));
            self.step_state(STEP_MQTT, StepState::Skipped);
            return (Outcome::Unverified, log, Some(msg));
        }
        line(&format!("UID: {uid}"), 300);
        self.step_state(STEP_RTT, StepState::Done);

        if !checks.mqtt {
            self.step_state(STEP_MQTT, StepState::Skipped);
            return (Outcome::Ok, log, None);
        }
        self.step_state(STEP_MQTT, StepState::Running);
        line("Modem: registering on the network", 800);
        if upper.contains("NOMQTT") {
            line("Network Connect Error: Timeout", MQTT_TIMEOUT_S * 1000);
            let msg = format!("no MQTT connection within {MQTT_TIMEOUT_S} s");
            self.step_state(STEP_MQTT, StepState::Failed(msg.clone()));
            return (Outcome::Unverified, log, Some(msg));
        }
        line("Network Connected", 1500);
        line(MARKER, 0);
        self.step_state(STEP_MQTT, StepState::Done);
        (Outcome::Ok, log, None)
    }
}

fn session_info() -> SessionInfo {
    SessionInfo {
        project: "Demo project".into(),
        variant: "Demo variant".into(),
        device_type: 0,
        bootloader: "bootloader-demo".into(),
        firmware: "firmware-demo".into(),
        ca_subject: "CN=Demo CA".into(),
        yubikey: "none (demo)".into(),
        dev_ca: false,
        probe: "none (demo)".into(),
        // Never written: the demo does not append to a log.
        log: "demo-issuance.csv".into(),
        uid_min: 5,
        uid_max: 9,
        exit_note: Some("Demo mode: nothing was flashed or logged.".into()),
        checks: Checks {
            iccid: true,
            mqtt: true,
        },
        mqtt_timeout_s: MQTT_TIMEOUT_S,
    }
}
