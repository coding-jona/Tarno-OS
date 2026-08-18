use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::thread;
use std::time::Duration;

use eframe::egui::{self, RichText};
use tarno_ui_theme as theme;

use crate::devices::{self, BlockDevice};
use crate::flasher::{self, CancelFlag, FlashEvent};

enum FlashState {
    Idle,
    Running { written: u64, total: u64, bytes_per_sec: f64 },
    Done { written: u64, elapsed_secs: f64 },
    Cancelled,
    Error(String),
}

pub struct InstallerApp {
    devices: Vec<BlockDevice>,
    selected: Option<usize>,

    image_path_input: String,
    image_size: Option<Result<u64, String>>,

    confirm_checked: bool,
    is_root: bool,

    flash_state: FlashState,
    events_rx: Option<Receiver<FlashEvent>>,
    cancel_flag: Option<CancelFlag>,

    screenshot_path: Option<PathBuf>,
    screenshot_frames: u32,
    screenshot_done: bool,
}

impl InstallerApp {
    pub fn new(screenshot_path: Option<PathBuf>) -> Self {
        Self {
            devices: devices::list_removable_devices(),
            selected: None,
            image_path_input: String::new(),
            image_size: None,
            confirm_checked: false,
            is_root: is_root(),
            flash_state: FlashState::Idle,
            events_rx: None,
            cancel_flag: None,
            screenshot_path,
            screenshot_frames: 0,
            screenshot_done: false,
        }
    }

    fn rescan_devices(&mut self) {
        self.devices = devices::list_removable_devices();
        if let Some(idx) = self.selected {
            if idx >= self.devices.len() {
                self.selected = None;
                self.confirm_checked = false;
            }
        }
    }

    fn check_image(&mut self) {
        let path = PathBuf::from(self.image_path_input.trim());
        self.image_size = Some(
            std::fs::metadata(&path)
                .map(|m| m.len())
                .map_err(|e| format!("{e}")),
        );
        self.confirm_checked = false;
    }

    fn start_flash(&mut self) {
        let Some(idx) = self.selected else { return };
        let Some(device) = self.devices.get(idx).cloned() else { return };
        let source = PathBuf::from(self.image_path_input.trim());

        let (tx, rx) = channel();
        let cancel = flasher::new_cancel_flag();
        self.events_rx = Some(rx);
        self.cancel_flag = Some(cancel.clone());
        self.flash_state = FlashState::Running { written: 0, total: 0, bytes_per_sec: 0.0 };

        thread::spawn(move || {
            flasher::flash(&source, &device.path, &tx, &cancel);
        });
    }

    fn cancel_flash(&self) {
        if let Some(flag) = &self.cancel_flag {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn drain_flash_events(&mut self) {
        let Some(rx) = &self.events_rx else { return };
        for event in rx.try_iter() {
            self.flash_state = match event {
                FlashEvent::Progress { written, total, bytes_per_sec } => {
                    FlashState::Running { written, total, bytes_per_sec }
                }
                FlashEvent::Done { written, elapsed_secs } => FlashState::Done { written, elapsed_secs },
                FlashEvent::Cancelled => FlashState::Cancelled,
                FlashEvent::Error(message) => FlashState::Error(message),
            };
        }
    }

    fn is_flashing(&self) -> bool {
        matches!(self.flash_state, FlashState::Running { .. })
    }

    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else { return };
        if self.screenshot_done {
            return;
        }
        self.screenshot_frames += 1;
        if self.screenshot_frames == 8 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Screenshot { image, .. } = event {
                    if let Err(e) = save_png(image, &path) {
                        eprintln!("tarno-installer: screenshot fehlgeschlagen: {e}");
                    } else {
                        eprintln!("tarno-installer: screenshot geschrieben nach {}", path.display());
                    }
                    self.screenshot_done = true;
                }
            }
        });
        if self.screenshot_done {
            std::process::exit(0);
        }
    }

    fn render_topbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .frame(egui::Frame::new().fill(theme::BG_PANEL).inner_margin(egui::Margin::symmetric(20, 14)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("TARNO").color(theme::ACCENT).strong().size(20.0));
                    ui.label(RichText::new("INSTALLER").color(theme::TEXT_MUTED).size(20.0));
                    ui.add_space(8.0);
                    ui.label(RichText::new("· USB-Boot-Image schreiben").color(theme::TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.is_root {
                            ui.label(
                                RichText::new(format!("● nicht {}", privilege_label()))
                                    .color(theme::WARNING)
                                    .strong(),
                            );
                        } else {
                            ui.label(RichText::new(format!("● {}", privilege_label())).color(theme::SUCCESS).strong());
                        }
                    });
                });
            });
    }

    fn render_root_warning(&self, ui: &mut egui::Ui) {
        if self.is_root {
            return;
        }
        theme::danger_card(ui, |ui| {
            ui.label(
                RichText::new(format!("{} erforderlich", privilege_label()))
                    .color(theme::DANGER)
                    .strong(),
            );
            #[cfg(windows)]
            let hint = "Zum Schreiben auf ein Laufwerk werden Administrator-Rechte benötigt. Rechtsklick auf tarno-installer.exe → \"Als Administrator ausführen\".";
            #[cfg(not(windows))]
            let hint = "Zum Schreiben auf ein Blockgerät wird Root benötigt. Mit z.B. `sudo tarno-installer` neu starten.";
            ui.label(RichText::new(hint).color(theme::TEXT_MUTED));
        });
    }

    fn render_image_section(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            ui.label(RichText::new("1. Image").color(theme::TEXT_MUTED).small());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.image_path_input)
                        .hint_text("/pfad/zu/tarno-os-sdcard.img")
                        .desired_width(420.0),
                );
                if ui.button("Prüfen").clicked() {
                    self.check_image();
                }
            });
            if let Some(result) = &self.image_size {
                ui.add_space(4.0);
                match result {
                    Ok(size) => ui.label(
                        RichText::new(format!("gültig — {}", flasher::format_bytes(*size))).color(theme::SUCCESS),
                    ),
                    Err(msg) => ui.label(RichText::new(format!("nicht lesbar: {msg}")).color(theme::DANGER)),
                };
            }
        });
    }

    fn render_device_section(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("2. Zielgerät").color(theme::TEXT_MUTED).small());
                if ui.small_button("↻ neu scannen").clicked() {
                    self.rescan_devices();
                }
            });
            ui.add_space(4.0);
            if self.devices.is_empty() {
                ui.label(
                    RichText::new("Kein Wechseldatenträger gefunden — Stick einstecken und neu scannen.")
                        .color(theme::TEXT_MUTED),
                );
                return;
            }
            for (idx, device) in self.devices.iter().enumerate() {
                let selected = self.selected == Some(idx);
                if ui.selectable_label(selected, device.label()).clicked() {
                    self.selected = Some(idx);
                    self.confirm_checked = false;
                }
            }
        });
    }

    fn render_confirm_section(&mut self, ui: &mut egui::Ui) {
        let Some(idx) = self.selected else { return };
        let Some(device) = self.devices.get(idx) else { return };
        let Some(Ok(image_size)) = self.image_size else { return };

        theme::danger_card(ui, |ui| {
            ui.label(RichText::new("3. Bestätigung").color(theme::DANGER).strong());
            ui.add_space(4.0);
            if image_size > device.size_bytes {
                ui.label(
                    RichText::new(format!(
                        "Image ({}) ist größer als das Zielgerät ({}) — kann nicht passen.",
                        flasher::format_bytes(image_size),
                        flasher::format_bytes(device.size_bytes)
                    ))
                    .color(theme::DANGER),
                );
                self.confirm_checked = false;
                return;
            }
            ui.checkbox(
                &mut self.confirm_checked,
                format!(
                    "Ich bestätige: ALLE Daten auf {} werden unwiderruflich überschrieben.",
                    device.label()
                ),
            );
        });
    }

    fn render_action_section(&mut self, ui: &mut egui::Ui) {
        theme::card(ui, |ui| {
            match &self.flash_state {
                FlashState::Idle | FlashState::Done { .. } | FlashState::Cancelled | FlashState::Error(_) => {
                    let can_flash = self.is_root
                        && self.selected.is_some()
                        && matches!(self.image_size, Some(Ok(_)))
                        && self.confirm_checked;
                    let clicked = ui
                        .add_enabled(
                            can_flash,
                            egui::Button::new(RichText::new("Flashen").color(theme::BG_APP)).fill(theme::ACCENT),
                        )
                        .clicked();
                    if clicked {
                        self.start_flash();
                    }
                }
                FlashState::Running { .. } => {
                    if ui.button("Abbrechen").clicked() {
                        self.cancel_flash();
                    }
                }
            }

            ui.add_space(8.0);
            match &self.flash_state {
                FlashState::Idle => {}
                FlashState::Running { written, total, bytes_per_sec } => {
                    let fraction = if *total > 0 { *written as f32 / *total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(fraction).show_percentage());
                    let remaining = (*total - *written).max(0);
                    let eta_secs = if *bytes_per_sec > 0.0 { remaining as f64 / *bytes_per_sec } else { 0.0 };
                    ui.label(
                        RichText::new(format!(
                            "{} / {} — {}/s — ETA {}",
                            flasher::format_bytes(*written),
                            flasher::format_bytes(*total),
                            flasher::format_bytes(*bytes_per_sec as u64),
                            format_duration(eta_secs)
                        ))
                        .color(theme::TEXT_MUTED)
                        .small(),
                    );
                }
                FlashState::Done { written, elapsed_secs } => {
                    ui.label(
                        RichText::new(format!(
                            "Fertig: {} in {} geschrieben. Stick jetzt sicher entfernen (ggf. `partprobe`, falls Partitionen nicht sofort erscheinen).",
                            flasher::format_bytes(*written),
                            format_duration(*elapsed_secs)
                        ))
                        .color(theme::SUCCESS),
                    );
                }
                FlashState::Cancelled => {
                    ui.label(RichText::new("Abgebrochen — Zielgerät kann unvollständig beschrieben sein.").color(theme::WARNING));
                }
                FlashState::Error(message) => {
                    ui.label(RichText::new(format!("Fehler: {message}")).color(theme::DANGER));
                }
            }
        });
    }
}

/// Prüft, ob der Prozess mit den nötigen Rechten für Rohschreibzugriff
/// läuft — unter Unix root (`geteuid() == 0`), unter Windows eine erhöhte
/// ("Als Administrator ausführen") Sitzung (siehe `win32::is_elevated`).
fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        crate::win32::is_elevated()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// UI-Label für die in `is_root()` geprüfte Berechtigung — "root" unter
/// Unix, "Administrator" unter Windows, damit die Warnung auf beiden
/// Plattformen korrekt beschriftet ist statt Unix-Jargon auf Windows
/// anzuzeigen.
fn privilege_label() -> &'static str {
    #[cfg(unix)]
    {
        "root"
    }
    #[cfg(windows)]
    {
        "Administrator"
    }
    #[cfg(not(any(unix, windows)))]
    {
        "root"
    }
}

fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "–".to_string();
    }
    let total = secs.round() as u64;
    let m = total / 60;
    let s = total % 60;
    if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn save_png(image: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let [width, height] = image.size;
    let mut bytes = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_array());
    }
    image::save_buffer(path, &bytes, width as u32, height as u32, image::ColorType::Rgba8)
        .map_err(|e| e.to_string())
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_flash_events();
        self.handle_screenshot(ctx);
        let repaint_delay = if self.is_flashing() { Duration::from_millis(100) } else { Duration::from_millis(400) };
        ctx.request_repaint_after(repaint_delay);

        self.render_topbar(ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG_APP).inner_margin(egui::Margin::symmetric(24, 20)))
            .show(ctx, |ui| {
                ui.label(RichText::new("USB-Stick beschreiben").size(22.0).strong().color(theme::TEXT_PRIMARY));
                ui.label(
                    RichText::new("Schreibt ein Tarno-OS-Boot-Image (siehe tarno-br2-external/) auf einen Wechseldatenträger.")
                        .color(theme::TEXT_MUTED),
                );
                ui.add_space(12.0);

                self.render_root_warning(ui);
                self.render_image_section(ui);
                self.render_device_section(ui);
                self.render_confirm_section(ui);
                self.render_action_section(ui);
            });
    }
}
