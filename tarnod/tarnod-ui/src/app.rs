use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use tarnod_protocol::{Request, Response};

use crate::client::{Client, ClientEvent};
use tarno_ui_theme as theme;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(PartialEq, Eq, Clone, Copy)]
enum Section {
    Dashboard,
    GamingMode,
    Security,
    ApiKeys,
}

/// Ordnet eintreffende Antworten (die IPC-Antwort selbst trägt keine
/// Request-ID) dem UI-Feld zu, das sie aktualisieren soll. Funktioniert,
/// weil `client.rs` Requests strikt seriell abarbeitet — Antworten kommen
/// exakt in Sende-Reihenfolge zurück.
enum PendingKind {
    PollGamingMode,
    PollSecurityStatus,
    SetGamingMode,
    ApiKey,
    Resume,
}

pub struct TarnodApp {
    client: Client,
    socket_path: PathBuf,
    section: Section,

    connected: bool,
    status_line: Option<String>,
    pending: VecDeque<PendingKind>,
    last_poll: Instant,

    isolated_cpus: Result<String, String>,
    gaming_mode_pending: bool,
    ebpf_active: Option<bool>,

    api_key_name: String,
    api_key_result: Option<Result<String, String>>,

    resume_pid_input: String,
    resume_result: Option<Result<i32, String>>,

    /// Nur für Verifikations-/Testzwecke (siehe main.rs): wenn gesetzt,
    /// wird nach einigen gerenderten Frames ein Screenshot geschrieben und
    /// die App beendet. Kein Teil des normalen Betriebs.
    screenshot_path: Option<PathBuf>,
    screenshot_frames: u32,
    screenshot_done: bool,
}

impl TarnodApp {
    pub fn new(socket_path: PathBuf, screenshot_path: Option<PathBuf>) -> Self {
        let client = Client::spawn(socket_path.clone());
        let section = match std::env::var("TARNOD_UI_SECTION").as_deref() {
            Ok("gaming-mode") => Section::GamingMode,
            Ok("security") => Section::Security,
            Ok("api-keys") => Section::ApiKeys,
            _ => Section::Dashboard,
        };
        Self {
            client,
            socket_path,
            section,
            connected: false,
            status_line: Some("verbinde …".to_string()),
            pending: VecDeque::new(),
            last_poll: Instant::now() - POLL_INTERVAL,
            isolated_cpus: Err("noch keine Daten".to_string()),
            gaming_mode_pending: false,
            ebpf_active: None,
            api_key_name: String::new(),
            api_key_result: None,
            resume_pid_input: String::new(),
            resume_result: None,
            screenshot_path,
            screenshot_frames: 0,
            screenshot_done: false,
        }
    }

    /// Fordert nach ein paar Frames (damit Layout/Verbindung sich
    /// stabilisieren) einen Screenshot an und schreibt ihn beim Eintreffen
    /// als PNG. Wird nur genutzt, wenn `TARNOD_UI_SCREENSHOT` gesetzt ist.
    fn handle_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else {
            return;
        };
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
                        eprintln!("tarnod-ui: screenshot fehlgeschlagen: {e}");
                    } else {
                        eprintln!("tarnod-ui: screenshot geschrieben nach {}", path.display());
                    }
                    self.screenshot_done = true;
                }
            }
        });

        if self.screenshot_done {
            std::process::exit(0);
        }
    }

    fn send(&mut self, kind: PendingKind, req: Request) {
        self.pending.push_back(kind);
        self.client.send(req);
    }

    fn handle_events(&mut self) {
        for event in self.client.poll_events() {
            match event {
                ClientEvent::Connected => {
                    self.connected = true;
                    self.status_line = None;
                    self.pending.clear();
                    self.last_poll = Instant::now() - POLL_INTERVAL;
                }
                ClientEvent::Disconnected(msg) => {
                    self.connected = false;
                    self.status_line = Some(msg);
                    self.pending.clear();
                    self.ebpf_active = None;
                }
                ClientEvent::Response(resp) => self.apply_response(resp),
            }
        }
    }

    fn apply_response(&mut self, resp: Response) {
        let Some(kind) = self.pending.pop_front() else {
            return;
        };
        match kind {
            PendingKind::PollGamingMode => {
                self.isolated_cpus = match resp {
                    Response::Ok { data } => Ok(data
                        .get("isolated_cpus")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()),
                    Response::Error { message } => Err(message),
                };
            }
            PendingKind::PollSecurityStatus => {
                if let Response::Ok { data } = resp {
                    self.ebpf_active = data.get("ebpf_active").and_then(|v| v.as_bool());
                }
            }
            PendingKind::SetGamingMode => {
                self.gaming_mode_pending = false;
                if let Response::Error { message } = resp {
                    self.status_line = Some(format!("Gaming-Mode-Umschaltung fehlgeschlagen: {message}"));
                }
            }
            PendingKind::ApiKey => {
                self.api_key_result = Some(match resp {
                    Response::Ok { data } => Ok(data
                        .get("value")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()),
                    Response::Error { message } => Err(message),
                });
            }
            PendingKind::Resume => {
                self.resume_result = Some(match resp {
                    Response::Ok { data } => Ok(data
                        .get("resumed")
                        .and_then(|v| v.as_i64())
                        .unwrap_or_default() as i32),
                    Response::Error { message } => Err(message),
                });
            }
        }
    }

    fn maybe_poll(&mut self) {
        if self.connected && self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = Instant::now();
            self.send(PendingKind::PollGamingMode, Request::GetGamingMode);
            self.send(PendingKind::PollSecurityStatus, Request::SecurityStatus);
        }
    }

    fn connection_badge(&self, ui: &mut egui::Ui) {
        let (color, text) = if self.connected {
            (theme::SUCCESS, "verbunden")
        } else {
            (theme::DANGER, "getrennt")
        };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, color);
            ui.label(RichText::new(text).color(color).strong());
        });
    }

    fn render_topbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_PANEL)
                    .inner_margin(egui::Margin::symmetric(20, 14)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("TARNO").color(theme::ACCENT).strong().size(20.0));
                    ui.label(RichText::new("OS").color(theme::TEXT_MUTED).size(20.0));
                    ui.add_space(8.0);
                    ui.label(RichText::new("· tarnod control").color(theme::TEXT_MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.connection_badge(ui);
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(self.socket_path.display().to_string())
                                .color(theme::TEXT_MUTED)
                                .monospace()
                                .small(),
                        );
                    });
                });
            });
    }

    fn render_nav(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(190.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_APP)
                    .inner_margin(egui::Margin::symmetric(12, 16)),
            )
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    self.nav_button(ui, Section::Dashboard, "◆  Dashboard");
                    self.nav_button(ui, Section::GamingMode, "▶  Gaming-Mode");
                    self.nav_button(ui, Section::Security, "◈  Security");
                    self.nav_button(ui, Section::ApiKeys, "🔑  API-Keys");
                });
            });
    }

    /// Windows-11-`NavigationView`-Stil: ausgewählter Eintrag bekommt eine
    /// dezente neutrale Fläche + einen schmalen Akzent-Balken am linken
    /// Rand, statt vollflächig akzentgefärbt zu sein (siehe
    /// `theme::paint_nav_selection_bar`).
    fn nav_button(&mut self, ui: &mut egui::Ui, section: Section, label: &str) {
        let selected = self.section == section;
        let text = if selected {
            RichText::new(label).color(theme::TEXT_PRIMARY).strong()
        } else {
            RichText::new(label).color(theme::TEXT_MUTED)
        };
        let button = egui::Button::new(text)
            .fill(if selected { theme::BG_PANEL } else { theme::BG_APP })
            .min_size(egui::vec2(ui.available_width(), 34.0));
        let response = ui.add(button);
        if selected {
            theme::paint_nav_selection_bar(ui, response.rect);
        }
        if response.clicked() {
            self.section = section;
        }
    }

    fn render_dashboard(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("Dashboard").size(22.0).strong().color(theme::TEXT_PRIMARY));
        ui.label(RichText::new("Überblick über Gaming-Mode und Security-Status.").color(theme::TEXT_MUTED));
        ui.add_space(12.0);

        ui.columns(2, |cols| {
            theme::card(&mut cols[0], |ui| {
                ui.label(RichText::new("GAMING-MODE").color(theme::TEXT_MUTED).small());
                ui.add_space(4.0);
                match &self.isolated_cpus {
                    Ok(cpus) if !cpus.is_empty() => {
                        ui.label(RichText::new(format!("isolierte CPUs: {cpus}")).color(theme::SUCCESS).strong());
                    }
                    Ok(_) => {
                        ui.label(RichText::new("kein isolcpus aktiv").color(theme::TEXT_PRIMARY));
                    }
                    Err(msg) => {
                        ui.label(RichText::new(msg).color(theme::WARNING).small());
                    }
                }
            });
            theme::card(&mut cols[1], |ui| {
                ui.label(RichText::new("SECURITY (eBPF)").color(theme::TEXT_MUTED).small());
                ui.add_space(4.0);
                match self.ebpf_active {
                    Some(true) => ui.label(RichText::new("● aktiv").color(theme::SUCCESS).strong()),
                    Some(false) => ui.label(RichText::new("○ inaktiv (Feature nicht gebaut)").color(theme::TEXT_MUTED)),
                    None => ui.label(RichText::new("unbekannt").color(theme::TEXT_MUTED)),
                };
            });
        });
    }

    fn render_gaming_mode(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("Gaming-Mode").size(22.0).strong().color(theme::TEXT_PRIMARY));
        ui.label(RichText::new("CPU-Governor performance/powersave umschalten (siehe scripts/gaming-mode.sh für den vollen Funktionsumfang inkl. THP).").color(theme::TEXT_MUTED));
        ui.add_space(12.0);

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                let on_clicked = ui
                    .add_enabled(
                        self.connected && !self.gaming_mode_pending,
                        egui::Button::new(RichText::new("Aktivieren").color(theme::BG_APP))
                            .fill(theme::ACCENT),
                    )
                    .clicked();
                let off_clicked = ui
                    .add_enabled(
                        self.connected && !self.gaming_mode_pending,
                        egui::Button::new("Deaktivieren"),
                    )
                    .clicked();
                if on_clicked {
                    self.gaming_mode_pending = true;
                    self.send(PendingKind::SetGamingMode, Request::SetGamingMode { enabled: true });
                }
                if off_clicked {
                    self.gaming_mode_pending = true;
                    self.send(PendingKind::SetGamingMode, Request::SetGamingMode { enabled: false });
                }
                if self.gaming_mode_pending {
                    ui.add(egui::Spinner::new().color(theme::ACCENT));
                }
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new("isolcpus-Status").color(theme::TEXT_MUTED).small());
            match &self.isolated_cpus {
                Ok(cpus) if !cpus.is_empty() => {
                    ui.label(RichText::new(cpus).monospace().color(theme::SUCCESS));
                }
                Ok(_) => {
                    ui.label(RichText::new("(leer — kein isolcpus=-Boot-Parameter aktiv)").color(theme::TEXT_MUTED));
                }
                Err(msg) => {
                    ui.label(RichText::new(msg).color(theme::WARNING));
                }
            }
        });
    }

    fn render_security(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("Security").size(22.0).strong().color(theme::TEXT_PRIMARY));
        ui.label(RichText::new("Behavioral-Security-Status (eBPF, Feature \"ebpf\") und manuelles Fortsetzen angehaltener Prozesse.").color(theme::TEXT_MUTED));
        ui.add_space(12.0);

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("eBPF-Guard:").color(theme::TEXT_MUTED));
                match self.ebpf_active {
                    Some(true) => ui.label(RichText::new("aktiv").color(theme::SUCCESS).strong()),
                    Some(false) => ui.label(RichText::new("inaktiv").color(theme::TEXT_MUTED)),
                    None => ui.label(RichText::new("unbekannt").color(theme::TEXT_MUTED)),
                };
            });
        });

        ui.add_space(12.0);
        theme::card(ui, |ui| {
            ui.label(RichText::new("Prozess fortsetzen (SIGCONT)").color(theme::TEXT_MUTED).small());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.resume_pid_input)
                        .hint_text("PID")
                        .desired_width(100.0),
                );
                let clicked = ui
                    .add_enabled(self.connected, egui::Button::new("Resume"))
                    .clicked();
                if clicked {
                    if let Ok(pid) = self.resume_pid_input.trim().parse::<i32>() {
                        self.send(PendingKind::Resume, Request::ResumeProcess { pid });
                    } else {
                        self.resume_result = Some(Err("ungültige PID".to_string()));
                    }
                }
            });
            if let Some(result) = &self.resume_result {
                ui.add_space(6.0);
                match result {
                    Ok(pid) => ui.label(RichText::new(format!("pid {pid} fortgesetzt")).color(theme::SUCCESS)),
                    Err(msg) => ui.label(RichText::new(msg).color(theme::DANGER)),
                };
            }
        });
    }

    fn render_api_keys(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("API-Keys").size(22.0).strong().color(theme::TEXT_PRIMARY));
        ui.label(RichText::new("Werte liegen ausschließlich im RAM des Daemons (siehe docs/month3-tarno-layer.md#api-key-vault) — hier nur zur Kontrolle abrufbar.").color(theme::TEXT_MUTED));
        ui.add_space(12.0);

        theme::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.api_key_name)
                        .hint_text("Key-Name, z.B. MOJANG_API_KEY")
                        .desired_width(240.0),
                );
                let clicked = ui
                    .add_enabled(self.connected && !self.api_key_name.trim().is_empty(), egui::Button::new("Abrufen"))
                    .clicked();
                if clicked {
                    self.send(
                        PendingKind::ApiKey,
                        Request::GetApiKey {
                            name: self.api_key_name.trim().to_string(),
                        },
                    );
                }
            });
            if let Some(result) = &self.api_key_result {
                ui.add_space(6.0);
                match result {
                    Ok(value) => ui.label(RichText::new(value).monospace().color(theme::SUCCESS)),
                    Err(msg) => ui.label(RichText::new(msg).color(theme::DANGER)),
                };
            }
        });
    }
}

/// Schreibt ein egui-`ColorImage` (RGBA, straight alpha) als PNG-Datei.
fn save_png(image: &egui::ColorImage, path: &std::path::Path) -> Result<(), String> {
    let [width, height] = image.size;
    let mut bytes = Vec::with_capacity(width * height * 4);
    for pixel in &image.pixels {
        bytes.extend_from_slice(&pixel.to_array());
    }
    image::save_buffer(
        path,
        &bytes,
        width as u32,
        height as u32,
        image::ColorType::Rgba8,
    )
    .map_err(|e| e.to_string())
}

impl eframe::App for TarnodApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        self.maybe_poll();
        self.handle_screenshot(ctx);
        ctx.request_repaint_after(Duration::from_millis(300));

        self.render_topbar(ctx);
        self.render_nav(ctx);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_APP)
                    .inner_margin(egui::Margin::symmetric(24, 20)),
            )
            .show(ctx, |ui| {
                if let Some(status) = self.status_line.clone() {
                    if !self.connected {
                        theme::card(ui, |ui| {
                            ui.label(RichText::new("Nicht verbunden").color(theme::WARNING).strong());
                            ui.label(RichText::new(status).color(theme::TEXT_MUTED).small());
                        });
                        ui.add_space(12.0);
                    }
                }
                match self.section {
                    Section::Dashboard => self.render_dashboard(ui),
                    Section::GamingMode => self.render_gaming_mode(ui),
                    Section::Security => self.render_security(ui),
                    Section::ApiKeys => self.render_api_keys(ui),
                }
            });
    }
}
