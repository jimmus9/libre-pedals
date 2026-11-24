use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use dsp::ParamSpec;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};
use engine::{
    list_devices, Engine, EngineConfig, EngineInfo, HostKind, HostReport, InitialParams,
    MeterSnapshot, Meters, Params, PedalParamSlot,
};

#[derive(Debug, Clone, Default)]
pub struct GuiStartOptions {
    pub host: Option<HostKind>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub mute: bool,
}

const BUFFER_OPTIONS: [Option<u32>; 6] = [
    None,
    Some(64),
    Some(128),
    Some(256),
    Some(512),
    Some(1024),
];

pub fn run_gui(opts: GuiStartOptions) -> anyhow::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pedalera Virtual — Guitar FX")
            .with_inner_size([1120.0, 640.0])
            .with_min_inner_size([780.0, 500.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Pedalera Virtual",
        native_options,
        Box::new(move |cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = Color32::from_rgb(16, 19, 24);
            visuals.extreme_bg_color = Color32::from_rgb(10, 12, 16);
            cc.egui_ctx.set_visuals(visuals);
            Ok(Box::new(PedalboardApp::new(opts)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Error al iniciar la ventana gráfica: {e}"))
}

struct PedalboardApp {
    params: Arc<Params>,
    meters: Arc<Meters>,
    engine: Option<Engine>,
    reports: Vec<HostReport>,

    selected_host: HostKind,
    selected_input: Option<String>,
    selected_output: Option<String>,
    selected_in_channel: usize,
    selected_buffer: Option<u32>,

    last_error: Option<String>,
    last_snapshot: MeterSnapshot,
    smooth_in_db: f32,
    smooth_out_db: f32,
    last_frame_time: Instant,
}

impl PedalboardApp {
    fn new(opts: GuiStartOptions) -> Self {
        let reports = list_devices();
        let selected_host = opts.host.unwrap_or(HostKind::Wasapi);
        let init = InitialParams {
            mute: opts.mute,
            ..Default::default()
        };
        let params = Arc::new(Params::from_initial(init));
        let meters = Arc::new(Meters::new());

        let mut app = Self {
            params,
            meters,
            engine: None,
            reports,
            selected_host,
            selected_input: opts.input,
            selected_output: opts.output,
            selected_in_channel: 0,
            selected_buffer: None,
            last_error: None,
            last_snapshot: MeterSnapshot {
                input_peak: 0.0,
                output_peak: 0.0,
                input_db: -120.0,
                output_db: -120.0,
                underruns: 0,
                overflows: 0,
                drift_drops: 0,
                stream_errors: 0,
            },
            smooth_in_db: -120.0,
            smooth_out_db: -120.0,
            last_frame_time: Instant::now(),
        };

        app.select_defaults_if_missing();
        app.start_engine();
        app
    }

    fn current_host_report(&self) -> Option<&HostReport> {
        self.reports.iter().find(|r| r.host == self.selected_host)
    }

    fn input_device_names(&self) -> Vec<String> {
        self.current_host_report()
            .map(|h| {
                h.devices
                    .iter()
                    .filter(|d| d.input_channels.unwrap_or(0) > 0)
                    .map(|d| d.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn output_device_names(&self) -> Vec<String> {
        self.current_host_report()
            .map(|h| {
                h.devices
                    .iter()
                    .filter(|d| d.output_channels.unwrap_or(0) > 0)
                    .map(|d| d.name.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn max_input_channels(&self) -> usize {
        let Some(host) = self.current_host_report() else {
            return 2;
        };
        if let Some(sel) = &self.selected_input {
            if let Some(dev) = host.devices.iter().find(|d| &d.name == sel) {
                return dev.input_channels.unwrap_or(2).max(1) as usize;
            }
        }
        2
    }

    fn select_defaults_if_missing(&mut self) {
        let (def_in_name, def_out_name) = {
            let Some(host) = self.current_host_report() else {
                return;
            };
            let def_in = host
                .devices
                .iter()
                .find(|d| d.is_default_input && d.input_channels.unwrap_or(0) > 0)
                .or_else(|| host.devices.iter().find(|d| d.input_channels.unwrap_or(0) > 0))
                .map(|d| d.name.clone());
            let def_out = host
                .devices
                .iter()
                .find(|d| d.is_default_output && d.output_channels.unwrap_or(0) > 0)
                .or_else(|| host.devices.iter().find(|d| d.output_channels.unwrap_or(0) > 0))
                .map(|d| d.name.clone());
            (def_in, def_out)
        };

        if self.selected_input.is_none() {
            self.selected_input = def_in_name;
        }

        #[cfg(feature = "asio")]
        if self.selected_host == HostKind::Asio {
            self.selected_output = self.selected_input.clone();
        }

        if self.selected_output.is_none() {
            self.selected_output = def_out_name;
        }

        let max_ch = self.max_input_channels();
        if self.selected_in_channel >= max_ch {
            self.selected_in_channel = 0;
        }
    }

    fn refresh_devices(&mut self) {
        // Detener temporalmente si es ASIO antes de re-enumerar, para liberar el driver único.
        #[cfg(feature = "asio")]
        let was_asio = self.selected_host == HostKind::Asio && self.engine.is_some();
        #[cfg(not(feature = "asio"))]
        let was_asio = false;

        if was_asio {
            self.engine = None;
        }

        self.reports = list_devices();
        self.select_defaults_if_missing();

        if was_asio {
            self.start_engine();
        }
    }

    fn start_engine(&mut self) {
        // Siempre soltar streams previos antes de abrir nuevos (crítico para ASIO).
        self.engine = None;
        self.last_error = None;

        #[cfg(feature = "asio")]
        if self.selected_host == HostKind::Asio {
            self.selected_output = self.selected_input.clone();
        }

        let max_ch = self.max_input_channels();
        if self.selected_in_channel >= max_ch {
            self.selected_in_channel = 0;
        }

        self.meters = Arc::new(Meters::new());
        let cfg = EngineConfig {
            host: self.selected_host,
            input_name: self.selected_input.clone(),
            output_name: self.selected_output.clone(),
            input_channel: self.selected_in_channel,
            buffer_frames: self.selected_buffer,
            latency_frames: None,
            params: Arc::clone(&self.params),
            meters: Arc::clone(&self.meters),
        };

        match Engine::start(cfg) {
            Ok(eng) => {
                self.engine = Some(eng);
                self.last_error = None;
            }
            Err(err) => {
                self.last_error = Some(format!("{err:#}"));
            }
        }
    }

    fn stop_engine(&mut self) {
        self.engine = None;
        self.smooth_in_db = -120.0;
        self.smooth_out_db = -120.0;
    }

    fn update_meters(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32().min(0.2);
        self.last_frame_time = now;

        if self.engine.is_some() {
            let snap = self.meters.snapshot_and_reset_peaks();
            self.last_snapshot = snap;
            let decay = 36.0 * dt; // caída visual ~36 dB/s
            self.smooth_in_db = if snap.input_db > self.smooth_in_db {
                snap.input_db
            } else {
                (self.smooth_in_db - decay).max(-120.0)
            };
            self.smooth_out_db = if snap.output_db > self.smooth_out_db {
                snap.output_db
            } else {
                (self.smooth_out_db - decay).max(-120.0)
            };
        } else {
            self.smooth_in_db = -120.0;
            self.smooth_out_db = -120.0;
        }
    }

    /// Devuelve el umbral activo del Noise Gate (para dibujar la marca sobre el vúmetro de entrada).
    fn active_gate_threshold_db(&self) -> Option<f32> {
        for slot in self.params.slots() {
            if slot.name == "Noise Gate" && !slot.bypass() {
                let th = slot.get(0);
                if th > -100.0 {
                    return Some(th);
                }
            }
        }
        None
    }

    fn draw_top_bar(&mut self, ui: &mut egui::Ui) {
        let panel_frame = Frame::new()
            .fill(Color32::from_rgb(22, 26, 34))
            .inner_margin(Margin::symmetric(14, 10))
            .stroke(Stroke::new(1.0, Color32::from_rgb(42, 48, 60)));

        egui::Panel::top("audio_header")
            .frame(panel_frame)
            .show(ui, |ui| {
                // Fila 1: Título + controles de dispositivo
                ui.horizontal_wrapped(|ui| {
                    ui.heading(
                        egui::RichText::new("🎸 Pedalera Virtual")
                            .strong()
                            .color(Color32::from_rgb(240, 244, 248)),
                    );
                    ui.separator();

                    // Selector de Host
                    ui.label("Host:");
                    let prev_host = self.selected_host;
                    egui::ComboBox::from_id_salt("host_select")
                        .selected_text(self.selected_host.display_name())
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.selected_host, HostKind::Wasapi, "WASAPI");
                            #[cfg(feature = "asio")]
                            ui.selectable_value(&mut self.selected_host, HostKind::Asio, "ASIO");
                        });
                    if self.selected_host != prev_host {
                        self.selected_input = None;
                        self.selected_output = None;
                        self.selected_in_channel = 0;
                        self.select_defaults_if_missing();
                        self.start_engine();
                    }

                    // Selector de Entrada
                    let in_names = self.input_device_names();
                    ui.label("Entrada:");
                    let in_label = self
                        .selected_input
                        .as_deref()
                        .unwrap_or("(sin entrada)");
                    egui::ComboBox::from_id_salt("in_dev_select")
                        .selected_text(truncate_label(in_label, 28))
                        .width(210.0)
                        .show_ui(ui, |ui| {
                            for name in &in_names {
                                ui.selectable_value(
                                    &mut self.selected_input,
                                    Some(name.clone()),
                                    name,
                                );
                            }
                        });

                    #[cfg(feature = "asio")]
                    if self.selected_host == HostKind::Asio {
                        self.selected_output = self.selected_input.clone();
                    }

                    // Selector de Canal de entrada
                    let max_ch = self.max_input_channels();
                    if self.selected_in_channel >= max_ch {
                        self.selected_in_channel = 0;
                    }
                    ui.label("Canal:");
                    egui::ComboBox::from_id_salt("in_ch_select")
                        .selected_text(format!("Ch {}", self.selected_in_channel))
                        .width(64.0)
                        .show_ui(ui, |ui| {
                            for ch in 0..max_ch {
                                ui.selectable_value(
                                    &mut self.selected_in_channel,
                                    ch,
                                    format!("Canal {ch}"),
                                );
                            }
                        });

                    // Selector de Salida
                    let out_names = self.output_device_names();
                    let is_asio = {
                        #[cfg(feature = "asio")]
                        {
                            self.selected_host == HostKind::Asio
                        }
                        #[cfg(not(feature = "asio"))]
                        {
                            false
                        }
                    };
                    ui.label("Salida:");
                    let out_label = truncate_label(
                        self.selected_output
                            .as_deref()
                            .unwrap_or("(sin salida)"),
                        28,
                    );
                    ui.add_enabled_ui(!is_asio, |ui| {
                        egui::ComboBox::from_id_salt("out_dev_select")
                            .selected_text(out_label)
                            .width(210.0)
                            .show_ui(ui, |ui| {
                                for name in &out_names {
                                    ui.selectable_value(
                                        &mut self.selected_output,
                                        Some(name.clone()),
                                        name,
                                    );
                                }
                            });
                    })
                    .response
                    .on_disabled_hover_text("En ASIO la salida usa el mismo driver que la entrada");

                    // Selector de Buffer
                    ui.label("Buffer:");
                    let buf_text = self
                        .selected_buffer
                        .map_or_else(|| "Auto".to_string(), |b| format!("{b} fr"));
                    egui::ComboBox::from_id_salt("buf_select")
                        .selected_text(buf_text)
                        .width(78.0)
                        .show_ui(ui, |ui| {
                            for opt in BUFFER_OPTIONS {
                                let label = opt.map_or_else(
                                    || "Auto (Default)".to_string(),
                                    |b| format!("{b} frames"),
                                );
                                ui.selectable_value(&mut self.selected_buffer, opt, label);
                            }
                        });
                });

                ui.add_space(6.0);

                // Fila 2: Botones de control de motor + Estado + MUTE
                ui.horizontal_wrapped(|ui| {
                    let running = self.engine.is_some();
                    let start_label = if running {
                        "🔄 Aplicar / Reconectar"
                    } else {
                        "▶ Iniciar Audio"
                    };
                    if ui
                        .add(
                            egui::Button::new(start_label)
                                .fill(Color32::from_rgb(34, 110, 72)),
                        )
                        .clicked()
                    {
                        self.start_engine();
                    }

                    if ui
                        .add_enabled(
                            running,
                            egui::Button::new("⏹ Detener")
                                .fill(Color32::from_rgb(80, 42, 46)),
                        )
                        .clicked()
                    {
                        self.stop_engine();
                    }

                    if ui.button("🔍 Refrescar dispositivos").clicked() {
                        self.refresh_devices();
                    }

                    let muted = self.params.mute();
                    let mute_btn = egui::Button::new(
                        egui::RichText::new(if muted { "🔇 MUTE ACTIVO" } else { "🔊 Salida Activa" })
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(if muted {
                        Color32::from_rgb(185, 45, 45)
                    } else {
                        Color32::from_rgb(45, 58, 76)
                    });
                    if ui.add(mute_btn).clicked() {
                        self.params.set_mute(!muted);
                    }

                    ui.separator();

                    if let Some(err) = &self.last_error {
                        ui.colored_label(Color32::from_rgb(248, 113, 113), format!("Error: {err}"));
                    } else if let Some(eng) = &self.engine {
                        let info = eng.info();
                        let sr_text = format_sample_rate(info);
                        let in_b = info
                            .input_buffer_frames
                            .map_or("?".to_string(), |v| v.to_string());
                        let out_b = info
                            .output_buffer_frames
                            .map_or("?".to_string(), |v| v.to_string());
                        let xruns = self.last_snapshot.underruns + self.last_snapshot.overflows;
                        ui.colored_label(
                            Color32::from_rgb(74, 222, 128),
                            "● EN LÍNEA",
                        );
                        ui.label(format!(
                            "{sr_text}  |  Buf: {in_b}/{out_b} + ring {} fr  |  Latencia: {:.1} ms  |  xruns: {xruns} (und:{}, ovf:{}, drift:{})",
                            info.prefill_frames,
                            info.estimated_latency_ms,
                            self.last_snapshot.underruns,
                            self.last_snapshot.overflows,
                            self.last_snapshot.drift_drops,
                        ));
                    } else {
                        ui.colored_label(Color32::from_rgb(160, 170, 185), "○ Motor detenido");
                    }
                });

                ui.add_space(6.0);

                // Fila 3: Vúmetros en vivo
                let gate_th = self.active_gate_threshold_db();
                let muted = self.params.mute();
                ui.columns(2, |cols| {
                    draw_level_meter(
                        &mut cols[0],
                        "ENTRADA",
                        self.smooth_in_db,
                        gate_th,
                        false,
                    );
                    draw_level_meter(
                        &mut cols[1],
                        "SALIDA",
                        self.smooth_out_db,
                        None,
                        muted,
                    );
                });
            });
    }

    fn draw_cascade_board(&mut self, ui: &mut egui::Ui) {
        let central_frame = Frame::new()
            .fill(Color32::from_rgb(15, 18, 23))
            .inner_margin(Margin::same(16));

        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("CASCADA DE EFECTOS")
                            .strong()
                            .size(15.0)
                            .color(Color32::from_rgb(203, 213, 225)),
                    );
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(
                            "Arrastra las perillas arriba/abajo o usa la rueda · Shift = ajuste fino · Doble clic = valor inicial",
                        )
                        .size(12.0)
                        .color(Color32::from_rgb(120, 134, 152)),
                    );
                });

                ui.add_space(12.0);

                let slot_count = self.params.slot_count();
                let in_label = self
                    .selected_input
                    .as_deref()
                    .unwrap_or("Sin entrada")
                    .to_string();
                let out_label = self
                    .selected_output
                    .as_deref()
                    .unwrap_or("Sin salida")
                    .to_string();
                let in_ch = self.selected_in_channel;
                let muted = self.params.mute();
                let active = self.engine.is_some();

                egui::ScrollArea::horizontal()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;

                            draw_endpoint_card(
                                ui,
                                "🎸 ENTRADA",
                                &truncate_label(&in_label, 22),
                                &format!("Canal {in_ch}"),
                                Color32::from_rgb(59, 130, 246),
                                active,
                            );

                            draw_signal_arrow(ui, active);

                            for cascade_pos in 0..slot_count {
                                let slot_idx = self.params.order_at(cascade_pos);
                                if let Some(slot) = self.params.slots().get(slot_idx) {
                                    draw_stompbox_pedal(
                                        ui,
                                        &self.params,
                                        cascade_pos,
                                        slot_count,
                                        slot,
                                    );
                                }
                                draw_signal_arrow(ui, active && !muted);
                            }

                            draw_endpoint_card(
                                ui,
                                if muted { "🔇 SALIDA (MUTE)" } else { "🔊 SALIDA" },
                                &truncate_label(&out_label, 22),
                                if muted { "Silenciada" } else { "Estéreo" },
                                if muted {
                                    Color32::from_rgb(220, 38, 38)
                                } else {
                                    Color32::from_rgb(34, 197, 94)
                                },
                                active && !muted,
                            );
                        });
                    });
            });
    }
}

impl eframe::App for PedalboardApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_meters();
        self.draw_top_bar(ui);
        self.draw_cascade_board(ui);
        ui.ctx()
            .request_repaint_after(Duration::from_millis(33));
    }
}

fn format_sample_rate(info: &EngineInfo) -> String {
    if info.input_sample_rate != info.output_sample_rate {
        format!(
            "{} -> {} Hz (resampling)",
            info.input_sample_rate, info.output_sample_rate
        )
    } else {
        format!("{} Hz", info.sample_rate)
    }
}

fn truncate_label(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

fn draw_level_meter(
    ui: &mut egui::Ui,
    title: &str,
    db: f32,
    marker_db: Option<f32>,
    muted: bool,
) {
    const MIN_DB: f32 = -60.0;
    const MAX_DB: f32 = 0.0;

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .small()
                .strong()
                .color(Color32::from_rgb(175, 185, 200)),
        );

        let desired = Vec2::new((ui.available_width() - 88.0).max(120.0), 16.0);
        let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
        let p = ui.painter();

        p.rect(
            rect,
            CornerRadius::same(4),
            Color32::from_rgb(10, 12, 16),
            Stroke::new(1.0, Color32::from_rgb(45, 52, 66)),
            StrokeKind::Inside,
        );

        let norm = ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0);
        if norm > 0.002 {
            let fill_rect = Rect::from_min_max(
                rect.min + Vec2::splat(2.0),
                Pos2::new(
                    rect.min.x + 2.0 + (rect.width() - 4.0) * norm,
                    rect.max.y - 2.0,
                ),
            );
            let bar_color = if muted {
                Color32::from_rgb(100, 100, 110)
            } else if db > -3.0 {
                Color32::from_rgb(239, 68, 68)
            } else if db > -12.0 {
                Color32::from_rgb(234, 179, 8)
            } else {
                Color32::from_rgb(34, 197, 94)
            };
            p.rect_filled(fill_rect, CornerRadius::same(2), bar_color);
        }

        // Marca visual del umbral del Noise Gate
        if let Some(th_db) = marker_db {
            let th_norm = ((th_db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0);
            let x = rect.min.x + 2.0 + (rect.width() - 4.0) * th_norm;
            p.line_segment(
                [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                Stroke::new(2.0, Color32::from_rgb(56, 189, 248)),
            );
        }

        let db_text = if muted {
            "MUTE".to_string()
        } else if db <= -119.0 {
            "-∞ dB".to_string()
        } else {
            format!("{db:>5.1} dB")
        };
        ui.monospace(db_text);
    });
}

fn draw_endpoint_card(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    detail: &str,
    accent: Color32,
    active: bool,
) {
    let border = if active {
        accent
    } else {
        Color32::from_rgb(58, 65, 78)
    };
    Frame::new()
        .fill(Color32::from_rgb(24, 28, 36))
        .corner_radius(CornerRadius::same(10))
        .stroke(Stroke::new(1.5, border))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(135.0, 260.0));
            ui.set_max_width(155.0);
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);
                ui.label(egui::RichText::new(title).strong().size(13.0).color(accent));
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new(subtitle)
                        .size(12.0)
                        .color(Color32::from_rgb(220, 226, 236)),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(detail)
                        .small()
                        .color(Color32::from_rgb(138, 150, 168)),
                );
            });
        });
}

fn draw_signal_arrow(ui: &mut egui::Ui, active: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(26.0, 40.0), Sense::hover());
    let color = if active {
        Color32::from_rgb(148, 163, 184)
    } else {
        Color32::from_rgb(65, 74, 88)
    };
    let y = rect.center().y;
    let p = ui.painter();
    p.line_segment(
        [Pos2::new(rect.min.x + 2.0, y), Pos2::new(rect.max.x - 4.0, y)],
        Stroke::new(2.5, color),
    );
    p.line_segment(
        [
            Pos2::new(rect.max.x - 10.0, y - 5.0),
            Pos2::new(rect.max.x - 3.0, y),
        ],
        Stroke::new(2.5, color),
    );
    p.line_segment(
        [
            Pos2::new(rect.max.x - 10.0, y + 5.0),
            Pos2::new(rect.max.x - 3.0, y),
        ],
        Stroke::new(2.5, color),
    );
}

fn draw_stompbox_pedal(
    ui: &mut egui::Ui,
    params: &Params,
    cascade_pos: usize,
    slot_count: usize,
    slot: &PedalParamSlot,
) {
    let [r, g, b] = slot.accent_color;
    let accent = Color32::from_rgb(r, g, b);
    let bypass = slot.bypass();
    let active = !bypass;

    let knob_count = slot.param_count().max(1);
    let pedal_width = (108.0 + (knob_count as f32) * 86.0).max(185.0);

    let border_color = if active {
        accent
    } else {
        Color32::from_rgb(62, 68, 80)
    };
    let bg_color = if active {
        Color32::from_rgb(30, 35, 45)
    } else {
        Color32::from_rgb(24, 27, 34)
    };

    Frame::new()
        .fill(bg_color)
        .corner_radius(CornerRadius::same(12))
        .stroke(Stroke::new(2.0, border_color))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(pedal_width, 280.0));
            ui.set_max_width(pedal_width);

            // Cabecera del pedal: posición en cascada, botones de orden y LED
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("#{}", cascade_pos + 1))
                        .small()
                        .strong()
                        .color(Color32::from_rgb(148, 163, 184)),
                );

                if ui
                    .add_enabled(cascade_pos > 0, egui::Button::new("◀").small())
                    .on_hover_text("Mover pedal antes en la cascada")
                    .clicked()
                {
                    params.swap_cascade_positions(cascade_pos, cascade_pos - 1);
                }

                if ui
                    .add_enabled(cascade_pos + 1 < slot_count, egui::Button::new("▶").small())
                    .on_hover_text("Mover pedal después en la cascada")
                    .clicked()
                {
                    params.swap_cascade_positions(cascade_pos, cascade_pos + 1);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (led_rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                    let c = led_rect.center();
                    let p = ui.painter();
                    if active {
                        p.circle_filled(
                            c,
                            8.0,
                            Color32::from_rgba_unmultiplied(r, g, b, 65),
                        );
                        p.circle_filled(c, 5.0, accent);
                    } else {
                        p.circle(
                            c,
                            5.0,
                            Color32::from_rgb(45, 50, 58),
                            Stroke::new(1.0, Color32::from_rgb(75, 82, 95)),
                        );
                    }
                });
            });

            ui.add_space(6.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(slot.name.to_uppercase())
                        .strong()
                        .size(15.0)
                        .color(if active {
                            Color32::from_rgb(241, 245, 249)
                        } else {
                            Color32::from_rgb(125, 134, 148)
                        }),
                );
            });

            ui.add_space(12.0);

            // Fila automática de perillas según los metadatos del pedal
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                for p_idx in 0..slot.param_count() {
                    let spec = &slot.specs[p_idx];
                    let mut val = slot.get(p_idx);
                    if draw_rotary_knob(ui, spec, &mut val, accent, active) {
                        slot.set(p_idx, val);
                    }
                }
            });

            ui.add_space(16.0);

            // Zona inferior: Footswitch + Reset
            ui.vertical_centered(|ui| {
                let switch_text = if active { "● ACTIVO" } else { "○ BYPASS" };
                let switch_fill = if active {
                    Color32::from_rgb(r / 2 + 25, g / 2 + 25, b / 2 + 25)
                } else {
                    Color32::from_rgb(46, 52, 62)
                };
                let switch_btn = egui::Button::new(
                    egui::RichText::new(switch_text)
                        .strong()
                        .size(13.0)
                        .color(Color32::WHITE),
                )
                .min_size(Vec2::new(124.0, 34.0))
                .corner_radius(CornerRadius::same(18))
                .fill(switch_fill);

                if ui.add(switch_btn).clicked() {
                    slot.set_bypass(!bypass);
                }

                ui.add_space(6.0);
                if ui
                    .add(egui::Button::new("↺ Reset").small())
                    .on_hover_text("Restaurar valores por defecto de este pedal")
                    .clicked()
                {
                    slot.reset_defaults();
                }
            });
        });
}

fn draw_rotary_knob(
    ui: &mut egui::Ui,
    spec: &ParamSpec,
    value: &mut f32,
    accent: Color32,
    enabled: bool,
) -> bool {
    let desired_size = Vec2::new(78.0, 106.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());

    let mut changed = false;

    if response.double_clicked() || response.secondary_clicked() {
        *value = spec.default;
        changed = true;
    } else {
        let mut norm = spec.to_normalized(*value);
        let mut delta = 0.0_f32;

        if response.dragged() {
            let sensitivity = if ui.input(|i| i.modifiers.shift) {
                0.0015
            } else {
                0.0055
            };
            delta -= response.drag_delta().y * sensitivity;
        }

        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                delta += scroll * 0.0012;
            }
        }

        if delta.abs() > 0.0 {
            norm = (norm + delta).clamp(0.0, 1.0);
            *value = spec.from_normalized(norm);
            changed = true;
        }
    }

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let norm = spec.to_normalized(*value);
        let center = Pos2::new(rect.center().x, rect.min.y + 52.0);
        let outer_radius = 27.0_f32;
        let body_radius = 20.0_f32;

        // Nombre del parámetro arriba
        let title_color = if enabled {
            Color32::from_rgb(203, 213, 225)
        } else {
            Color32::from_rgb(115, 125, 140)
        };
        p.text(
            Pos2::new(rect.center().x, rect.min.y + 10.0),
            Align2::CENTER_CENTER,
            spec.name,
            FontId::proportional(12.5),
            title_color,
        );

        let start_angle = -0.75 * std::f32::consts::PI; // -135° desde las 12 en punto
        let sweep = 1.5 * std::f32::consts::PI; // 270° totales
        let active_angle = start_angle + norm * sweep;

        // Arco de fondo
        let bg_points: Vec<Pos2> = (0..=28)
            .map(|i| {
                let t = i as f32 / 28.0;
                let a = start_angle + t * sweep;
                center + Vec2::new(a.sin(), -a.cos()) * outer_radius
            })
            .collect();
        p.line(bg_points, Stroke::new(3.5, Color32::from_rgb(48, 55, 68)));

        // Arco activo
        let active_steps = ((norm * 28.0).ceil() as usize).max(1);
        if norm > 0.005 {
            let active_points: Vec<Pos2> = (0..=active_steps)
                .map(|i| {
                    let t = i as f32 / active_steps as f32;
                    let a = start_angle + t * (active_angle - start_angle);
                    center + Vec2::new(a.sin(), -a.cos()) * outer_radius
                })
                .collect();
            let arc_color = if enabled {
                accent
            } else {
                Color32::from_rgb(95, 105, 120)
            };
            p.line(active_points, Stroke::new(3.5, arc_color));
        }

        // Cuerpo de la perilla
        let body_fill = if response.hovered() || response.dragged() {
            Color32::from_rgb(46, 52, 64)
        } else {
            Color32::from_rgb(36, 41, 51)
        };
        p.circle(
            center,
            body_radius,
            body_fill,
            Stroke::new(1.5, Color32::from_rgb(78, 88, 104)),
        );

        // Indicador / puntero de la perilla
        let dir = Vec2::new(active_angle.sin(), -active_angle.cos());
        let pointer_color = if enabled {
            Color32::WHITE
        } else {
            Color32::from_rgb(140, 148, 160)
        };
        p.line_segment(
            [center + dir * 5.0, center + dir * (body_radius - 3.0)],
            Stroke::new(2.5, pointer_color),
        );

        // Valor formateado debajo
        p.text(
            Pos2::new(rect.center().x, rect.max.y - 12.0),
            Align2::CENTER_CENTER,
            spec.format_value(*value),
            FontId::monospace(11.5),
            if enabled {
                Color32::from_rgb(226, 232, 240)
            } else {
                Color32::from_rgb(120, 130, 145)
            },
        );
    }

    changed
}
