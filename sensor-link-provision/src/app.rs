//! egui front-end: a setup screen (artifacts, variant, log, PIN) and the
//! per-device loop screen. All work happens in the worker thread.

use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender, channel},
};

use eframe::egui::{self, Color32, RichText};

use crate::{
    artifacts::Artifacts,
    log_csv, sound, validate,
    worker::{self, Command, DevCa, Event, Outcome, SessionConfig, SessionInfo, StepState},
};

pub struct App {
    tx: Sender<Command>,
    rx: Receiver<Event>,
    sounds: sound::Sounds,
    logo: Option<egui::TextureHandle>,
    about_open: bool,
    /// Native menu bar; kept alive for the app's lifetime (macOS only).
    #[cfg(target_os = "macos")]
    _menu: Option<muda::Menu>,
    screen: Screen,
}

/// Build the macOS application menu with an About item and the standard
/// Hide/Quit entries, and install it.
#[cfg(target_os = "macos")]
fn build_app_menu() -> Option<muda::Menu> {
    use muda::{Menu, MenuItem, PredefinedMenuItem, Submenu};
    let menu = Menu::new();
    let app = Submenu::new("sensor-link-provision", true);
    let about = MenuItem::with_id("about", "About sensor-link-provision", true, None);
    app.append_items(&[
        &about,
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::quit(None),
    ])
    .ok()?;
    menu.append(&app).ok()?;
    menu.init_for_nsapp();
    Some(menu)
}

/// Aspect ratio of the Jitter wordmark (viewBox 160x50).
const LOGO_ASPECT: f32 = 160.0 / 50.0;

/// Jitter blue, used for section badges and accents.
const ACCENT: Color32 = Color32::from_rgb(77, 159, 220);

fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(include_bytes!("../assets/jitter-logo.png"))
        .ok()?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Some(ctx.load_texture("jitter-logo", color, egui::TextureOptions::LINEAR))
}

fn logo(ui: &mut egui::Ui, tex: Option<&egui::TextureHandle>, height: f32) {
    if let Some(tex) = tex {
        ui.add(
            egui::Image::from_texture(tex)
                .fit_to_exact_size(egui::vec2(height * LOGO_ASPECT, height)),
        );
    }
}

enum Screen {
    Setup(Setup),
    Session(Session),
}

#[derive(Default)]
struct Setup {
    zip: Option<PathBuf>,
    artifacts: Option<Result<Artifacts, String>>,
    variant: usize,
    log: String,
    pin: String,
    ca_cert_file: Option<PathBuf>,
    /// Set from the command line only.
    dev_ca: Option<DevCa>,
    probes: Option<Vec<String>>,
    starting: bool,
    error: Option<String>,
}

struct Session {
    info: SessionInfo,
    uid: String,
    icc: String,
    focus_uid: bool,
    focus_icc: bool,
    counts: Counts,
    phase: Phase,
    last: Option<Finished>,
    steps: Vec<StepState>,
    /// Flashing progress (0.0..=1.0) per step, for the progress bars.
    progress: Vec<f32>,
    /// RTT boot log streamed for the device currently being verified.
    live_rtt: String,
    show_summary: bool,
}

#[derive(Default)]
struct Counts {
    ok: u32,
    unverified: u32,
    fail: u32,
}

enum Phase {
    Idle,
    /// Soft-check warnings the operator must acknowledge before starting.
    Confirm {
        warnings: Vec<String>,
        uid: String,
        icc: String,
        reprovision: bool,
    },
    Running,
    /// A step failed; waiting for Retry / Skip.
    Failed {
        step: usize,
        message: String,
    },
}

struct Finished {
    uid: String,
    outcome: Outcome,
    error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, dev_ca: Option<DevCa>) -> Self {
        let (tx, cmd_rx) = channel();
        let (ev_tx, rx) = channel();
        worker::spawn(cmd_rx, ev_tx, cc.egui_ctx.clone());
        cc.egui_ctx.set_zoom_factor(1.15);
        let _ = tx.send(Command::ListProbes);
        Self {
            tx,
            rx,
            sounds: sound::Sounds::new(),
            logo: load_logo(&cc.egui_ctx),
            about_open: false,
            #[cfg(target_os = "macos")]
            _menu: build_app_menu(),
            screen: Screen::Setup(Setup {
                dev_ca,
                ..Default::default()
            }),
        }
    }

    fn handle_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Probes(list) => {
                    if let Screen::Setup(s) = &mut self.screen {
                        s.probes = Some(list);
                    }
                }
                Event::SessionReady(info) => {
                    self.screen = Screen::Session(Session {
                        info: *info,
                        uid: String::new(),
                        icc: String::new(),
                        focus_uid: true,
                        focus_icc: false,
                        counts: Counts::default(),
                        phase: Phase::Idle,
                        last: None,
                        steps: vec![StepState::Pending; worker::STEPS.len()],
                        progress: vec![0.0; worker::STEPS.len()],
                        live_rtt: String::new(),
                        show_summary: false,
                    });
                }
                Event::SessionFailed(msg) => {
                    if let Screen::Setup(s) = &mut self.screen {
                        s.starting = false;
                        s.error = Some(msg);
                    }
                }
                Event::Step { index, state } => {
                    if let Screen::Session(s) = &mut self.screen {
                        if let StepState::Failed(m) = &state
                            && index != worker::STEPS.len() - 1
                        {
                            s.phase = Phase::Failed {
                                step: index,
                                message: m.clone(),
                            };
                        }
                        if matches!(s.steps[index], StepState::Pending | StepState::Running)
                            && let Some(p) = s.progress.get_mut(index)
                        {
                            *p = 0.0;
                        }
                        s.steps[index] = state;
                    }
                }
                Event::Rtt(log) => {
                    if let Screen::Session(s) = &mut self.screen {
                        s.live_rtt = log;
                    }
                }
                Event::StepProgress { index, fraction } => {
                    if let Screen::Session(s) = &mut self.screen
                        && let Some(p) = s.progress.get_mut(index)
                    {
                        *p = fraction;
                    }
                }
                Event::DeviceFinished {
                    uid,
                    outcome,
                    reprovision,
                    rtt_log,
                    error,
                } => {
                    if let Screen::Session(s) = &mut self.screen {
                        if !reprovision {
                            match outcome {
                                Outcome::Ok => s.counts.ok += 1,
                                Outcome::Unverified => s.counts.unverified += 1,
                                Outcome::Fail => s.counts.fail += 1,
                            }
                        }
                        match outcome {
                            Outcome::Ok => self.sounds.ok(),
                            _ => self.sounds.fail(),
                        }
                        s.live_rtt = rtt_log;
                        s.last = Some(Finished {
                            uid,
                            outcome,
                            error,
                        });
                        s.phase = Phase::Idle;
                        s.uid.clear();
                        s.icc.clear();
                        s.focus_uid = true;
                    }
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        while let Ok(ev) = muda::MenuEvent::receiver().try_recv() {
            if ev.id == "about" {
                self.about_open = true;
            }
        }
        self.handle_events();
        egui::CentralPanel::default().show(ctx, |ui| match &mut self.screen {
            Screen::Setup(setup) => setup.ui(ui, &self.tx, self.logo.as_ref()),
            Screen::Session(session) => session.ui(ui, &self.tx, self.logo.as_ref()),
        });
        self.about_ui(ctx);
    }
}

impl App {
    fn about_ui(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        egui::Window::new("About")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.strong("sensor-link provisioning");
                ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                ui.add_space(8.0);
                ui.label(
                    "Provisioning tool for sensor-link devices: flashes the bootloader, \
                     firmware and per-device config over a J-Link, and signs each device \
                     certificate with a YubiKey-held CA.",
                );
                ui.add_space(8.0);
                ui.hyperlink_to("jitter.nl", "https://www.jitter.nl");
                ui.add_space(10.0);
                if ui.button("Close").clicked() {
                    self.about_open = false;
                }
            });
    }
}

impl Setup {
    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        tx: &Sender<Command>,
        logo_tex: Option<&egui::TextureHandle>,
    ) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(10.0);
                logo(ui, logo_tex, 48.0);
                ui.add_space(6.0);
                ui.heading("Provisioning");
                ui.add_space(20.0);

                section(ui, 1, "Firmware", |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("Select firmware zip\u{2026}").clicked()
                            && let Some(p) = rfd::FileDialog::new()
                                .add_filter("zip", &["zip"])
                                .pick_file()
                        {
                            self.artifacts = Some(Artifacts::load(&p).map_err(|e| format!("{e:#}")));
                            self.zip = Some(p);
                            self.variant = 0;
                            if let Some(Ok(a)) = &self.artifacts
                                && self.log.is_empty()
                                && let Some(d) = a.profile.default_log_path()
                            {
                                self.log = d.display().to_string();
                            }
                        }
                        if let Some(z) = &self.zip {
                            ui.label(
                                z.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default(),
                            );
                        }
                    });
                    match &self.artifacts {
                        None => {
                            ui.add_space(6.0);
                            ui.weak("The CI artifact zip (firmware-build-<run>.zip) with provision.toml, bootloader and firmware.");
                        }
                        Some(Err(e)) => {
                            ui.add_space(6.0);
                            ui.colored_label(Color32::RED, e);
                        }
                        Some(Ok(a)) => {
                            ui.add_space(10.0);
                            egui::Grid::new("profile")
                                .num_columns(2)
                                .spacing([24.0, 8.0])
                                .show(ui, |ui| {
                                    ui.label("Project");
                                    ui.strong(&a.profile.project.name);
                                    ui.end_row();
                                    ui.label("Bootloader");
                                    ui.label(&a.bootloader.name);
                                    ui.end_row();
                                    ui.label("Variant");
                                    egui::ComboBox::from_id_salt("variant")
                                        .selected_text(&a.profile.variants[self.variant].name)
                                        .show_ui(ui, |ui| {
                                            for (i, v) in a.profile.variants.iter().enumerate() {
                                                ui.selectable_value(&mut self.variant, i, &v.name);
                                            }
                                        });
                                    ui.end_row();
                                    ui.label("Firmware");
                                    ui.label(&a.firmwares[self.variant].name);
                                    ui.end_row();
                                    ui.label("Chip");
                                    ui.label(&a.profile.target.chip);
                                    ui.end_row();
                                    ui.label("Cert subject");
                                    ui.label(format!("{}, CN=<UID>", a.profile.identity.cert_subject));
                                    ui.end_row();
                                    ui.label("CA slot");
                                    ui.label(format!("YubiKey PIV {}", a.profile.identity.ca_piv_slot));
                                    ui.end_row();
                                });
                        }
                    }
                });

                section(ui, 2, "Secrets", |ui| {
                    egui::Grid::new("secrets")
                        .num_columns(2)
                        .spacing([24.0, 12.0])
                        .show(ui, |ui| {
                            ui.label("Issuance log (CSV)");
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.log).desired_width(360.0),
                                );
                                if ui.button("\u{2026}").clicked()
                                    && let Some(p) = rfd::FileDialog::new()
                                        .add_filter("csv", &["csv"])
                                        .save_file()
                                {
                                    self.log = p.display().to_string();
                                }
                            });
                            ui.end_row();

                            if let Some(dev) = &self.dev_ca {
                                ui.label("CA");
                                ui.vertical(|ui| {
                                    dev_ca_banner(ui);
                                    ui.label(format!("key: {}", dev.key.display()));
                                    ui.label(format!("cert: {}", dev.cert.display()));
                                });
                                ui.end_row();
                            } else {
                                ui.label("YubiKey PIV PIN");
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.pin)
                                        .password(true)
                                        .desired_width(200.0),
                                );
                                ui.end_row();
                            }
                        });

                    // The CA certificate is normally read from the YubiKey
                    // slot; the file picker is a rarely-needed fallback, so
                    // keep it collapsed unless a file has already been chosen.
                    if self.dev_ca.is_none() {
                        ui.add_space(6.0);
                        egui::CollapsingHeader::new("CA certificate not on the YubiKey")
                            .default_open(self.ca_cert_file.is_some())
                            .show(ui, |ui| {
                                let current = self.ca_cert_file.as_ref().map(|p| {
                                    p.file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default()
                                });
                                ui.horizontal(|ui| {
                                    if ui.button("Select file\u{2026}").clicked()
                                        && let Some(p) = rfd::FileDialog::new()
                                            .add_filter("PEM", &["pem", "cert", "crt"])
                                            .pick_file()
                                    {
                                        self.ca_cert_file = Some(p);
                                    }
                                    match current {
                                        Some(name) => {
                                            if ui.button("Clear").clicked() {
                                                self.ca_cert_file = None;
                                            }
                                            ui.label(name);
                                        }
                                        None => {
                                            ui.weak("Use only if the CA certificate is not stored in the YubiKey slot.");
                                        }
                                    }
                                });
                            });
                    }
                });

                section(ui, 3, "Start", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Probe:");
                        match &self.probes {
                            None => {
                                ui.spinner();
                            }
                            Some(p) if p.is_empty() => {
                                ui.colored_label(Color32::from_rgb(200, 120, 0), "none found");
                            }
                            Some(p) => {
                                ui.label(p.join(", "));
                            }
                        }
                        if ui.button("Rescan").clicked() {
                            self.probes = None;
                            let _ = tx.send(Command::ListProbes);
                        }
                    });
                    ui.add_space(14.0);
                    let ready = matches!(self.artifacts, Some(Ok(_)))
                        && !self.log.trim().is_empty()
                        && (!self.pin.is_empty() || self.dev_ca.is_some())
                        && !self.starting;
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(RichText::new("Start session").size(18.0))
                                .min_size(egui::vec2(190.0, 38.0)),
                        )
                        .clicked()
                        && let Some(zip) = &self.zip
                    {
                        self.starting = true;
                        self.error = None;
                        let _ = tx.send(Command::StartSession(SessionConfig {
                            zip: zip.clone(),
                            variant: self.variant,
                            log: crate::profile::expand_home(self.log.trim()),
                            pin: self.pin.clone(),
                            ca_cert_file: self.ca_cert_file.clone(),
                            dev_ca: self.dev_ca.clone(),
                        }));
                    }
                    if self.starting {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking YubiKey, CA and artifacts\u{2026}");
                        });
                    }
                    if let Some(e) = &self.error {
                        ui.add_space(6.0);
                        ui.colored_label(Color32::RED, e);
                    }
                });
            });
    }
}
impl Session {
    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        tx: &Sender<Command>,
        logo_tex: Option<&egui::TextureHandle>,
    ) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            logo(ui, logo_tex, 28.0);
            ui.heading(format!("{}: {}", self.info.project, self.info.variant));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("End session").clicked() {
                    self.show_summary = true;
                }
            });
        });
        ui.small(format!(
            "{} + {} (device type {}) | probe {} | CA {} | YubiKey {} | log {}",
            self.info.bootloader,
            self.info.firmware,
            self.info.device_type,
            self.info.probe,
            self.info.ca_subject,
            self.info.yubikey,
            self.info.log.display()
        ));
        if self.info.dev_ca {
            dev_ca_banner(ui);
        }
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            counter(ui, "OK", self.counts.ok, Color32::from_rgb(40, 160, 60));
            counter(
                ui,
                "UNVERIFIED",
                self.counts.unverified,
                Color32::from_rgb(210, 140, 0),
            );
            counter(
                ui,
                "FAILED",
                self.counts.fail,
                Color32::from_rgb(200, 40, 40),
            );
        });
        ui.add_space(4.0);

        let idle = matches!(self.phase, Phase::Idle);
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        let scan_color = Color32::from_rgb(45, 120, 220);
        let mut uid_focused = false;
        let mut icc_focused = false;
        ui.add_enabled_ui(idle, |ui| {
            egui::Grid::new("inputs")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Device UID").size(16.0));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.uid)
                            .id_salt("uid_field")
                            .font(egui::TextStyle::Heading)
                            .desired_width(260.0),
                    );
                    uid_focused = r.has_focus();
                    if self.focus_uid {
                        r.request_focus();
                        // Keep requesting until focus actually lands: a widget
                        // re-enabled this frame (Running -> Idle) can drop a
                        // single request_focus.
                        if r.has_focus() {
                            self.focus_uid = false;
                        }
                    }
                    // Enter (barcode-scanner suffix) submits: advance to the SIM field.
                    if r.lost_focus() && enter && !self.uid.trim().is_empty() {
                        self.uid = validate::normalize_uid(&self.uid);
                        self.focus_icc = true;
                    }
                    ui.end_row();
                    ui.label(RichText::new("SIM ICCID").size(16.0));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut self.icc)
                            .id_salt("icc_field")
                            .font(egui::TextStyle::Heading)
                            .desired_width(260.0),
                    );
                    icc_focused = r.has_focus();
                    if self.focus_icc {
                        r.request_focus();
                        if r.has_focus() {
                            self.focus_icc = false;
                        }
                    }
                    // Enter on the SIM field starts provisioning (hands-free scan flow).
                    if r.lost_focus() && enter && !self.icc.trim().is_empty() {
                        self.prepare_start();
                    }
                    ui.end_row();
                });
            if idle {
                if uid_focused {
                    ui.label(
                        RichText::new("\u{2b07} Scan the device UID barcode")
                            .color(scan_color)
                            .strong(),
                    );
                } else if icc_focused {
                    ui.label(
                        RichText::new("\u{2b07} Scan the SIM barcode (ICCID)")
                            .color(scan_color)
                            .strong(),
                    );
                }
            }
            ui.horizontal(|ui| {
                if ui.button("Provision").clicked() {
                    self.prepare_start();
                }
                if ui.button("Clear").clicked() {
                    self.uid.clear();
                    self.icc.clear();
                    self.focus_uid = true;
                }
            });
        });

        ui.add_space(10.0);
        ui.separator();
        self.status_ui(ui);

        ui.separator();
        for (i, name) in worker::STEPS.iter().enumerate() {
            ui.horizontal(|ui| {
                match &self.steps[i] {
                    StepState::Pending => {
                        ui.label(RichText::new("○").weak());
                    }
                    StepState::Running => {
                        ui.spinner();
                    }
                    StepState::Done => {
                        ui.colored_label(Color32::from_rgb(40, 160, 60), "✔");
                    }
                    StepState::Failed(_) => {
                        ui.colored_label(Color32::from_rgb(200, 40, 40), "✘");
                    }
                };
                ui.label(*name);
                if matches!(self.steps[i], StepState::Running) && self.progress[i] > 0.001 {
                    ui.add(
                        egui::ProgressBar::new(self.progress[i])
                            .desired_width(140.0)
                            .show_percentage(),
                    );
                }
                if let StepState::Failed(m) = &self.steps[i] {
                    ui.colored_label(Color32::from_rgb(200, 40, 40), m);
                }
            });
        }

        ui.add_space(8.0);
        ui.separator();
        // Terminal-style panel: near-black background, light monospace text.
        let terminal_bg = Color32::from_rgb(18, 18, 18);
        let terminal_fg = Color32::from_rgb(220, 223, 228);
        egui::Frame::new()
            .fill(terminal_bg)
            .inner_margin(egui::Margin::same(8))
            .corner_radius(4)
            .show(ui, |ui| {
                ui.style_mut().visuals.override_text_color = Some(terminal_fg);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if self.live_rtt.is_empty() {
                            ui.label(
                                RichText::new(
                                    "(RTT output from the device appears here during boot verification)",
                                )
                                .monospace()
                                .color(Color32::from_rgb(120, 124, 130)),
                            );
                        } else {
                            ui.label(RichText::new(&self.live_rtt).monospace().color(terminal_fg));
                        }
                    });
            });

        self.dialogs(&ctx, tx);
    }

    fn clear_for_new_device(&mut self) {
        self.live_rtt.clear();
    }

    fn prepare_start(&mut self) {
        self.clear_for_new_device();
        self.uid = validate::normalize_uid(&self.uid);
        self.icc = self.icc.trim().to_owned();
        if self.uid.is_empty() {
            self.focus_uid = true;
            return;
        }
        if self.icc.is_empty() {
            self.focus_icc = true;
            return;
        }
        let mut warnings = Vec::new();
        if let Some(w) = validate::uid_warning(&self.uid, self.info.uid_min, self.info.uid_max) {
            warnings.push(w);
        }
        if !validate::iccid_valid(&self.icc) {
            warnings.push(validate::ICCID_WARNING.to_owned());
        }
        let reprovision = log_csv::contains_uid(&self.info.log, &self.uid);
        if reprovision {
            warnings.push(format!(
                "UID {} is already in the log. Re-provision it?",
                self.uid
            ));
        }
        self.phase = Phase::Confirm {
            warnings,
            uid: self.uid.clone(),
            icc: self.icc.clone(),
            reprovision,
        };
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        match &self.phase {
            Phase::Running => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(format!("Provisioning {}…", self.uid)).size(22.0));
                });
            }
            Phase::Failed { step, message } => {
                ui.label(
                    RichText::new(format!("{} failed", worker::STEPS[*step]))
                        .size(22.0)
                        .color(Color32::from_rgb(200, 40, 40)),
                );
                ui.label(message);
            }
            _ => match &self.last {
                None => {
                    ui.label(RichText::new("Ready").size(22.0));
                }
                Some(f) => {
                    let (text, color) = match f.outcome {
                        Outcome::Ok => (format!("OK: {}", f.uid), Color32::from_rgb(40, 160, 60)),
                        Outcome::Unverified => (
                            format!("UNVERIFIED: {}", f.uid),
                            Color32::from_rgb(210, 140, 0),
                        ),
                        Outcome::Fail => {
                            (format!("FAIL: {}", f.uid), Color32::from_rgb(200, 40, 40))
                        }
                    };
                    ui.label(RichText::new(text).size(26.0).strong().color(color));
                    match f.outcome {
                        Outcome::Unverified => {
                            ui.label("Flashed and logged, but the boot log did not confirm the UID. Re-provision if the device does not work.");
                        }
                        Outcome::Fail => {
                            ui.label("Not provisioned. You can retry the same UID.");
                        }
                        Outcome::Ok => {}
                    }
                    if let Some(e) = &f.error {
                        ui.label(e);
                    }
                }
            },
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, tx: &Sender<Command>) {
        let mut next: Option<Phase> = None;
        match &self.phase {
            Phase::Confirm {
                warnings,
                uid,
                icc,
                reprovision,
            } => {
                if warnings.is_empty() {
                    let _ = tx.send(Command::Provision {
                        uid: uid.clone(),
                        icc: icc.clone(),
                        reprovision: *reprovision,
                    });
                    next = Some(Phase::Running);
                } else {
                    egui::Window::new("Please check")
                        .collapsible(false)
                        .resizable(false)
                        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                        .show(ctx, |ui| {
                            for w in warnings {
                                ui.label(w);
                                ui.add_space(4.0);
                            }
                            ui.horizontal(|ui| {
                                if ui.button("Cancel").clicked() {
                                    next = Some(Phase::Idle);
                                    self.focus_uid = true;
                                }
                                if ui.button("Continue anyway").clicked() {
                                    let _ = tx.send(Command::Provision {
                                        uid: uid.clone(),
                                        icc: icc.clone(),
                                        reprovision: *reprovision,
                                    });
                                    next = Some(Phase::Running);
                                }
                            });
                        });
                }
            }
            Phase::Failed { step, message } => {
                egui::Window::new("Step failed").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.strong(worker::STEPS[*step]);
                    ui.label(message);
                    ui.add_space(6.0);
                    ui.label("Fix the cause (probe, power, SWD plug) and retry, or skip this device.");
                    ui.horizontal(|ui| {
                        if ui.button("Retry").clicked() {
                            let _ = tx.send(Command::Retry);
                            next = Some(Phase::Running);
                        }
                        if ui.button("Skip device").clicked() {
                            let _ = tx.send(Command::Skip);
                            next = Some(Phase::Running);
                        }
                    });
                });
            }
            _ => {}
        }
        if let Some(p) = next {
            self.phase = p;
        }

        if self.show_summary {
            egui::Window::new("Session summary").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.label(format!("OK: {}   UNVERIFIED: {}   FAILED: {}", self.counts.ok, self.counts.unverified, self.counts.fail));
                ui.label(format!("Log: {}", self.info.log.display()));
                if let Some(note) = &self.info.exit_note {
                    ui.add_space(6.0);
                    ui.strong(note);
                }
                if self.counts.unverified > 0 {
                    ui.add_space(6.0);
                    ui.colored_label(
                        Color32::from_rgb(210, 140, 0),
                        format!("{} device(s) flashed but not verified over RTT; re-provision them if they do not work.", self.counts.unverified),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Back").clicked() {
                        self.show_summary = false;
                    }
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        }
    }
}

fn section(ui: &mut egui::Ui, n: u32, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 13.0, ACCENT);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            n.to_string(),
            egui::FontId::proportional(15.0),
            Color32::WHITE,
        );
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(19.0).strong());
    });
    ui.add_space(8.0);
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    ui.add_space(22.0);
}

fn dev_ca_banner(ui: &mut egui::Ui) {
    ui.label(
        RichText::new("DEVELOPMENT CA from file: certificates are not signed by the production CA")
            .strong()
            .color(Color32::from_rgb(210, 140, 0)),
    );
}

fn counter(ui: &mut egui::Ui, label: &str, n: u32, color: Color32) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new(n.to_string())
                    .size(30.0)
                    .strong()
                    .color(color),
            );
            ui.label(label);
        });
    });
}
