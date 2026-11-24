use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use dsp::ParamSpec;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Pos2, Rect, Sense, Stroke,
    StrokeKind, Vec2,
};
use engine::{
    list_devices, Engine, EngineConfig, HostKind, HostReport, InitialParams, MeterSnapshot, Meters,
    Params, PedalParamSlot, Preset, MAX_MOUNTED_PEDALS, PRESET_LEVEL_MAX_DB, PRESET_LEVEL_MIN_DB,
};

use crate::{
    analyzer_ui::AnalyzerState,
    presets::{
        delete_user_preset, list_user_presets, load_factory_preset, resolve_preset,
        save_user_preset, user_presets_dir, FACTORY,
    },
    settings::{host_from_str, host_to_string, PresetRef, Settings},
};

#[derive(Debug, Clone, Default)]
pub struct GuiStartOptions {
    pub host: Option<HostKind>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub mute: bool,
}

const BUFFER_OPTIONS: [Option<u32>; 6] =
    [None, Some(64), Some(128), Some(256), Some(512), Some(1024)];

const PROC_RATE_OPTIONS: [Option<u32>; 5] = [
    None,
    Some(44_100),
    Some(48_000),
    Some(96_000),
    Some(192_000),
];

pub fn run_gui(opts: GuiStartOptions) -> anyhow::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Pedalera Virtual — Guitar FX & Analizador")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Pedalera Virtual",
        native_options,
        Box::new(move |cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = Color32::from_rgb(15, 18, 23);
            visuals.extreme_bg_color = Color32::from_rgb(10, 12, 16);
            cc.egui_ctx.set_visuals(visuals);

            cc.egui_ctx.global_style_mut(|style| {
                style.spacing.button_padding = Vec2::new(10.0, 6.0);
                style.spacing.interact_size = Vec2::new(40.0, 28.0);
                style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            });

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
    analyzer: AnalyzerState,

    selected_host: HostKind,
    selected_input: Option<String>,
    selected_output: Option<String>,
    selected_in_channel: usize,
    selected_buffer: Option<u32>,
    selected_proc_rate: Option<u32>,

    last_error: Option<String>,
    resolution_warning: Option<String>,
    last_snapshot: MeterSnapshot,
    smooth_in_db: f32,
    smooth_out_db: f32,
    last_frame_time: Instant,

    settings_saved: Settings,
    settings_dirty_since: Option<Instant>,
    current_preset: Option<PresetRef>,
    loaded_snapshot: Option<Preset>,
    user_presets: Vec<(String, PathBuf)>,
    preset_status: Option<String>,
    preset_warnings: Vec<String>,
    save_as_name: Option<String>,
    confirm_overwrite: Option<String>,
    confirm_delete: Option<(String, PathBuf)>,
    pending_switch: Option<PresetRef>,
}

impl PedalboardApp {
    fn new(opts: GuiStartOptions) -> Self {
        let reports = list_devices();
        let settings = Settings::load();
        let selected_host = opts
            .host
            .or_else(|| host_from_str(settings.host.as_deref()))
            .unwrap_or(HostKind::Wasapi);
        let selected_input = opts.input.or_else(|| settings.input.clone());
        let selected_output = opts.output.or_else(|| settings.output.clone());
        let selected_in_channel = settings.in_channel;
        let selected_buffer = settings.buffer_frames;
        let selected_proc_rate = settings.processing_sample_rate;

        let init = InitialParams {
            mute: opts.mute,
            volume_db: settings
                .master_volume_db
                .unwrap_or(InitialParams::default().volume_db),
            ..Default::default()
        };
        let params = Arc::new(Params::from_initial(init));
        let meters = Arc::new(Meters::new());
        let user_presets = list_user_presets();

        let mut current_preset = None;
        let mut loaded_snapshot = None;
        let mut preset_status = None;
        let mut preset_warnings = Vec::new();

        if let Some(ref pref) = settings.last_preset {
            let loaded = load_preset_from_ref(pref, &user_presets);
            match loaded {
                Ok(preset) => {
                    preset_warnings = preset.apply(&params);
                    loaded_snapshot = Some(Preset::capture(&params, &preset.name));
                    current_preset = Some(pref.clone());
                }
                Err(err) => {
                    preset_status = Some(format!(
                        "No se pudo restaurar el último preset '{}': {err}",
                        pref.name
                    ));
                }
            }
        }

        let mut app = Self {
            params,
            meters,
            engine: None,
            reports,
            analyzer: AnalyzerState::new(),
            selected_host,
            selected_input,
            selected_output,
            selected_in_channel,
            selected_buffer,
            selected_proc_rate,
            last_error: None,
            resolution_warning: None,
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
            settings_saved: settings,
            settings_dirty_since: None,
            current_preset,
            loaded_snapshot,
            user_presets,
            preset_status,
            preset_warnings,
            save_as_name: None,
            confirm_overwrite: None,
            confirm_delete: None,
            pending_switch: None,
        };

        app.select_defaults_if_missing();
        app.start_engine();
        app
    }

    fn current_host_report(&self) -> Option<&HostReport> {
        self.reports.iter().find(|r| r.host == self.selected_host)
    }

    fn input_devices_with_rates(&self) -> Vec<(String, Option<u32>)> {
        self.current_host_report()
            .map(|h| {
                h.devices
                    .iter()
                    .filter(|d| d.input_channels.unwrap_or(0) > 0)
                    .map(|d| {
                        (
                            d.name.clone(),
                            d.default_input.as_ref().map(|c| c.sample_rate),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn output_devices_with_rates(&self) -> Vec<(String, Option<u32>)> {
        self.current_host_report()
            .map(|h| {
                h.devices
                    .iter()
                    .filter(|d| d.output_channels.unwrap_or(0) > 0)
                    .map(|d| {
                        (
                            d.name.clone(),
                            d.default_output.as_ref().map(|c| c.sample_rate),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_input_native_rate(&self) -> Option<u32> {
        let host = self.current_host_report()?;
        let sel = self.selected_input.as_ref()?;
        host.devices
            .iter()
            .find(|d| &d.name == sel)
            .and_then(|d| d.default_input.as_ref().map(|c| c.sample_rate))
    }

    fn selected_output_native_rate(&self) -> Option<u32> {
        let host = self.current_host_report()?;
        let sel = self.selected_output.as_ref()?;
        host.devices
            .iter()
            .find(|d| &d.name == sel)
            .and_then(|d| d.default_output.as_ref().map(|c| c.sample_rate))
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
        let (def_in_name, def_in_rate, def_out_name) = {
            let Some(host) = self.current_host_report() else {
                return;
            };
            let def_in_dev = host
                .devices
                .iter()
                .find(|d| d.is_default_input && d.input_channels.unwrap_or(0) > 0)
                .or_else(|| {
                    host.devices
                        .iter()
                        .find(|d| d.input_channels.unwrap_or(0) > 0)
                });
            let def_in_name = def_in_dev.map(|d| d.name.clone());
            let in_rate = self
                .selected_input
                .as_ref()
                .and_then(|n| host.devices.iter().find(|d| &d.name == n))
                .or(def_in_dev)
                .and_then(|d| d.default_input.as_ref().map(|c| c.sample_rate));

            let sys_def_out = host
                .devices
                .iter()
                .find(|d| d.is_default_output && d.output_channels.unwrap_or(0) > 0);
            let same_rate_out = in_rate.and_then(|sr| {
                if sys_def_out.and_then(|d| d.default_output.as_ref().map(|c| c.sample_rate))
                    == Some(sr)
                {
                    sys_def_out
                } else {
                    host.devices.iter().find(|d| {
                        d.output_channels.unwrap_or(0) > 0
                            && d.default_output.as_ref().map(|c| c.sample_rate) == Some(sr)
                    })
                }
            });
            let def_out_name = same_rate_out
                .or(sys_def_out)
                .or_else(|| {
                    host.devices
                        .iter()
                        .find(|d| d.output_channels.unwrap_or(0) > 0)
                })
                .map(|d| d.name.clone());

            (def_in_name, in_rate, def_out_name)
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

        if let (Some(req), Some(in_sr)) = (self.selected_proc_rate, def_in_rate) {
            if req > in_sr {
                self.selected_proc_rate = None;
            }
        }

        let max_ch = self.max_input_channels();
        if self.selected_in_channel >= max_ch {
            self.selected_in_channel = 0;
        }
    }

    fn refresh_devices(&mut self) {
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
        self.engine = None;
        self.last_error = None;

        #[cfg(feature = "asio")]
        if self.selected_host == HostKind::Asio {
            self.selected_output = self.selected_input.clone();
        }

        if let (Some(req_sr), Some(in_sr)) =
            (self.selected_proc_rate, self.selected_input_native_rate())
        {
            if req_sr > in_sr {
                self.resolution_warning = Some(format!(
                    "⚠ Resolución {req_sr} Hz supera la frecuencia nativa de entrada ({in_sr} Hz). Se usa nativa ({in_sr} Hz)."
                ));
                self.selected_proc_rate = None;
            }
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
            processing_sample_rate: self.selected_proc_rate,
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

    fn update_meters(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_frame_time)
            .as_secs_f32()
            .min(0.2);
        self.last_frame_time = now;

        let proc_sr = self
            .engine
            .as_ref()
            .map(|e| e.info().processing_sample_rate)
            .or(self.selected_proc_rate)
            .or_else(|| self.selected_input_native_rate())
            .unwrap_or(48_000);

        self.analyzer
            .update_from_engine(ctx, self.engine.as_mut(), proc_sr);

        if self.engine.is_some() {
            let snap = self.meters.snapshot_and_reset_peaks();
            self.last_snapshot = snap;
            let decay = 36.0 * dt;
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

        self.maybe_save_settings();
    }

    fn current_settings(&self) -> Settings {
        Settings {
            host: Some(host_to_string(self.selected_host)),
            input: self.selected_input.clone(),
            output: self.selected_output.clone(),
            in_channel: self.selected_in_channel,
            buffer_frames: self.selected_buffer,
            processing_sample_rate: self.selected_proc_rate,
            master_volume_db: Some(self.params.volume_db()),
            last_preset: self.current_preset.clone(),
        }
    }

    fn maybe_save_settings(&mut self) {
        let current = self.current_settings();
        if current != self.settings_saved {
            let now = Instant::now();
            match self.settings_dirty_since {
                None => self.settings_dirty_since = Some(now),
                Some(since) if now.duration_since(since) >= Duration::from_secs(1) => {
                    if let Err(err) = current.save() {
                        eprintln!("Aviso al guardar settings.json: {err}");
                    } else {
                        self.settings_saved = current;
                    }
                    self.settings_dirty_since = None;
                }
                _ => {}
            }
        } else {
            self.settings_dirty_since = None;
        }
    }

    fn is_preset_dirty(&self) -> bool {
        let Some(ref snap) = self.loaded_snapshot else {
            return false;
        };
        !Preset::capture(&self.params, &snap.name).same_sound(snap)
    }

    fn all_preset_refs(&self) -> Vec<PresetRef> {
        let mut list: Vec<PresetRef> = FACTORY
            .iter()
            .map(|&(name, _)| PresetRef {
                source: "factory".to_string(),
                name: name.to_string(),
            })
            .collect();
        for (name, _) in &self.user_presets {
            list.push(PresetRef {
                source: "user".to_string(),
                name: name.clone(),
            });
        }
        list
    }

    fn request_load_preset(&mut self, pref: PresetRef) {
        if self.current_preset.as_ref() == Some(&pref) && !self.is_preset_dirty() {
            return;
        }
        if self.is_preset_dirty() {
            self.pending_switch = Some(pref);
        } else {
            self.apply_preset_ref(&pref);
        }
    }

    fn apply_preset_ref(&mut self, pref: &PresetRef) {
        match load_preset_from_ref(pref, &self.user_presets) {
            Ok(preset) => {
                self.preset_warnings = preset.apply(&self.params);
                self.preset_status = None;
                self.loaded_snapshot = Some(Preset::capture(&self.params, &preset.name));
                self.current_preset = Some(pref.clone());
            }
            Err(err) => {
                self.preset_status = Some(format!("Error al cargar preset '{}': {err}", pref.name));
            }
        }
    }

    fn save_preset_with_name(&mut self, name: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            self.preset_status = Some("Escribe un nombre válido para el preset.".to_string());
            return;
        }
        let preset = Preset::capture(&self.params, trimmed);
        match save_user_preset(&preset) {
            Ok(_) => {
                self.user_presets = list_user_presets();
                self.current_preset = Some(PresetRef {
                    source: "user".to_string(),
                    name: trimmed.to_string(),
                });
                self.loaded_snapshot = Some(preset);
                self.preset_status = None;
                self.preset_warnings.clear();
            }
            Err(err) => {
                self.preset_status = Some(format!("Error al guardar preset: {err}"));
            }
        }
    }

    fn draw_preset_bar(&mut self, ui: &mut egui::Ui) {
        let bar_frame = Frame::new()
            .fill(Color32::from_rgb(14, 17, 23))
            .inner_margin(Margin::symmetric(14, 6))
            .stroke(Stroke::new(1.0, Color32::from_rgb(34, 41, 54)));

        egui::Panel::top("preset_bar")
            .frame(bar_frame)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("PRESET")
                            .strong()
                            .size(13.0)
                            .color(Color32::from_rgb(148, 163, 184)),
                    );

                    let all_refs = self.all_preset_refs();
                    let current_idx = self
                        .current_preset
                        .as_ref()
                        .and_then(|cur| all_refs.iter().position(|r| r == cur));

                    if ui
                        .add_enabled(
                            !all_refs.is_empty(),
                            egui::Button::new("◀").min_size(Vec2::new(28.0, 26.0)),
                        )
                        .on_hover_text("Preset anterior")
                        .clicked()
                    {
                        let next_idx = match current_idx {
                            Some(i) => (i + all_refs.len() - 1) % all_refs.len(),
                            None => all_refs.len() - 1,
                        };
                        self.request_load_preset(all_refs[next_idx].clone());
                    }

                    let selected_label = match &self.current_preset {
                        Some(p) if p.source == "factory" => format!("🏭 {}", p.name),
                        Some(p) => p.name.clone(),
                        None => "(Personalizado / Sin preset)".to_string(),
                    };

                    let mut clicked_ref: Option<PresetRef> = None;
                    egui::ComboBox::from_id_salt("preset_select")
                        .selected_text(selected_label)
                        .width(210.0)
                        .show_ui(ui, |ui| {
                            for &(name, _) in FACTORY {
                                let candidate = PresetRef {
                                    source: "factory".to_string(),
                                    name: name.to_string(),
                                };
                                let selected = self.current_preset.as_ref() == Some(&candidate);
                                if ui
                                    .selectable_label(selected, format!("🏭 {name}"))
                                    .clicked()
                                {
                                    clicked_ref = Some(candidate);
                                }
                            }
                            if !self.user_presets.is_empty() {
                                ui.separator();
                                for (name, _) in &self.user_presets {
                                    let candidate = PresetRef {
                                        source: "user".to_string(),
                                        name: name.clone(),
                                    };
                                    let selected = self.current_preset.as_ref() == Some(&candidate);
                                    if ui.selectable_label(selected, name).clicked() {
                                        clicked_ref = Some(candidate);
                                    }
                                }
                            }
                        });
                    if let Some(pref) = clicked_ref {
                        self.request_load_preset(pref);
                    }

                    if ui
                        .add_enabled(
                            !all_refs.is_empty(),
                            egui::Button::new("▶").min_size(Vec2::new(28.0, 26.0)),
                        )
                        .on_hover_text("Siguiente preset")
                        .clicked()
                    {
                        let next_idx = match current_idx {
                            Some(i) => (i + 1) % all_refs.len(),
                            None => 0,
                        };
                        self.request_load_preset(all_refs[next_idx].clone());
                    }

                    if self.is_preset_dirty() {
                        ui.label(
                            egui::RichText::new("●")
                                .strong()
                                .size(15.0)
                                .color(Color32::from_rgb(251, 191, 36)),
                        )
                        .on_hover_text("Cambios sin guardar");
                    }

                    if ui
                        .add(egui::Button::new("💾 Guardar").min_size(Vec2::new(88.0, 26.0)))
                        .clicked()
                    {
                        if let Some(ref cur) = self.current_preset {
                            if cur.source == "user" {
                                let name = cur.name.clone();
                                self.save_preset_with_name(&name);
                            } else {
                                self.save_as_name = Some(format!("{} (copia)", cur.name));
                            }
                        } else {
                            self.save_as_name = Some("Mi Preset".to_string());
                        }
                    }

                    if ui
                        .add(egui::Button::new("Guardar como…").min_size(Vec2::new(112.0, 26.0)))
                        .clicked()
                    {
                        let suggested = self
                            .current_preset
                            .as_ref()
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| "Mi Preset".to_string());
                        self.save_as_name = Some(suggested);
                    }

                    let user_path = self.current_preset.as_ref().and_then(|cur| {
                        if cur.source == "user" {
                            self.user_presets
                                .iter()
                                .find(|(n, _)| n.eq_ignore_ascii_case(&cur.name))
                                .map(|(n, p)| (n.clone(), p.clone()))
                        } else {
                            None
                        }
                    });
                    if ui
                        .add_enabled(
                            user_path.is_some(),
                            egui::Button::new("🗑 Eliminar").min_size(Vec2::new(88.0, 26.0)),
                        )
                        .on_hover_text("Eliminar el preset de usuario activo")
                        .clicked()
                    {
                        if let Some(pair) = user_path {
                            self.confirm_delete = Some(pair);
                        }
                    }

                    if ui
                        .add(egui::Button::new("📂 Carpeta").min_size(Vec2::new(86.0, 26.0)))
                        .on_hover_text("Abrir la carpeta de presets de usuario")
                        .clicked()
                    {
                        match user_presets_dir() {
                            Ok(dir) => {
                                if let Err(err) =
                                    std::process::Command::new("explorer").arg(&dir).spawn()
                                {
                                    self.preset_status = Some(format!(
                                        "No se pudo abrir '{}': {err}",
                                        dir.display()
                                    ));
                                }
                            }
                            Err(err) => {
                                self.preset_status = Some(format!("{err}"));
                            }
                        }
                    }

                    ui.separator();
                    ui.label("Nivel:");
                    let mut lvl = self.params.preset_level_db();
                    if ui
                        .add(
                            egui::DragValue::new(&mut lvl)
                                .range(PRESET_LEVEL_MIN_DB..=PRESET_LEVEL_MAX_DB)
                                .speed(0.1)
                                .max_decimals(1)
                                .suffix(" dB"),
                        )
                        .on_hover_text("Nivel propio del preset (el Master permanece fijo)")
                        .changed()
                    {
                        self.params.set_preset_level_db(lvl);
                    }
                });

                if !self.preset_warnings.is_empty() || self.preset_status.is_some() {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        let mut parts = Vec::new();
                        if let Some(ref st) = self.preset_status {
                            parts.push(st.clone());
                        }
                        parts.extend(self.preset_warnings.iter().cloned());
                        ui.colored_label(
                            Color32::from_rgb(251, 191, 36),
                            format!("⚠ {}", parts.join(" · ")),
                        );
                        if ui.button("Cerrar").clicked() {
                            self.preset_status = None;
                            self.preset_warnings.clear();
                        }
                    });
                }
            });
    }

    fn draw_preset_dialogs(&mut self, ui: &mut egui::Ui) {
        if let Some(mut name_buf) = self.save_as_name.take() {
            let mut keep_open = true;
            let mut submit = false;
            egui::Window::new("Guardar preset como")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label("Nombre del preset:");
                    let te = ui.text_edit_singleline(&mut name_buf);
                    if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        submit = true;
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Guardar").clicked() {
                            submit = true;
                        }
                        if ui.button("Cancelar").clicked() {
                            keep_open = false;
                        }
                    });
                });

            if submit {
                let trimmed = name_buf.trim().to_string();
                if trimmed.is_empty() {
                    self.preset_status = Some("El nombre no puede estar vacío.".to_string());
                    self.save_as_name = Some(name_buf);
                } else if self
                    .user_presets
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case(&trimmed))
                {
                    self.confirm_overwrite = Some(trimmed);
                } else {
                    self.save_preset_with_name(&trimmed);
                }
            } else if keep_open {
                self.save_as_name = Some(name_buf);
            }
        }

        if let Some(name) = self.confirm_overwrite.take() {
            let mut keep_open = true;
            egui::Window::new("Confirmar sobrescritura")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Ya existe un preset llamado '{name}'. ¿Deseas sobrescribirlo?"
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Sobrescribir").clicked() {
                            self.save_preset_with_name(&name);
                            keep_open = false;
                        }
                        if ui.button("Cancelar").clicked() {
                            self.save_as_name = Some(name.clone());
                            keep_open = false;
                        }
                    });
                });
            if keep_open {
                self.confirm_overwrite = Some(name);
            }
        }

        if let Some((name, path)) = self.confirm_delete.take() {
            let mut keep_open = true;
            egui::Window::new("Eliminar preset")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "¿Eliminar el preset '{name}'? Esta acción no se puede deshacer."
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Eliminar").clicked() {
                            match delete_user_preset(&path) {
                                Ok(()) => {
                                    self.user_presets = list_user_presets();
                                    if self
                                        .current_preset
                                        .as_ref()
                                        .is_some_and(|c| c.source == "user" && c.name == name)
                                    {
                                        self.current_preset = None;
                                        self.loaded_snapshot = None;
                                    }
                                    self.preset_status = None;
                                }
                                Err(err) => {
                                    self.preset_status =
                                        Some(format!("Error al eliminar preset: {err}"));
                                }
                            }
                            keep_open = false;
                        }
                        if ui.button("Cancelar").clicked() {
                            keep_open = false;
                        }
                    });
                });
            if keep_open {
                self.confirm_delete = Some((name, path));
            }
        }

        if let Some(target) = self.pending_switch.take() {
            let mut keep_open = true;
            egui::Window::new("Cambios sin guardar")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "El preset actual tiene cambios sin guardar. ¿Descartarlos y cargar '{}'?",
                        target.name
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Descartar y cambiar").clicked() {
                            self.apply_preset_ref(&target);
                            keep_open = false;
                        }
                        if ui.button("Cancelar").clicked() {
                            keep_open = false;
                        }
                    });
                });
            if keep_open {
                self.pending_switch = Some(target);
            }
        }
    }

    fn active_gate_threshold_db(&self) -> Option<f32> {
        for slot in self.params.slots() {
            if slot.kind == "noise_gate" && slot.is_mounted() && !slot.bypass() {
                let th = slot.get(0);
                if th > -100.0 {
                    return Some(th);
                }
            }
        }
        None
    }

    fn draw_studio_rack(&mut self, ui: &mut egui::Ui) {
        let rack_frame = Frame::new()
            .fill(Color32::from_rgb(18, 22, 29))
            .inner_margin(Margin::symmetric(14, 8))
            .stroke(Stroke::new(1.0, Color32::from_rgb(38, 45, 58)));

        egui::Panel::top("studio_rack")
            .frame(rack_frame)
            .show(ui, |ui| {
                ui.columns(3, |cols| {
                    self.draw_input_rack_block(&mut cols[0]);
                    self.draw_engine_rack_block(&mut cols[1]);
                    self.draw_master_output_rack_block(&mut cols[2]);
                });

                if let Some(warn) = &self.resolution_warning.clone() {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::from_rgb(251, 191, 36), warn);
                        if ui.button("Cerrar aviso").clicked() {
                            self.resolution_warning = None;
                        }
                    });
                }
            });
    }

    fn draw_input_rack_block(&mut self, ui: &mut egui::Ui) {
        solid_rack_frame(Color32::from_rgb(59, 130, 246)).show(ui, |ui| {
            ui.set_min_height(132.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("🎸 ENTRADA DE AUDIO")
                        .strong()
                        .size(13.5)
                        .color(Color32::from_rgb(96, 165, 250)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    rack_fixed_badge(ui, "FIJO");
                });
            });
            ui.add_space(4.0);

            let prev_host = self.selected_host;
            let prev_buf = self.selected_buffer;
            let prev_in = self.selected_input.clone();
            let prev_ch = self.selected_in_channel;

            ui.horizontal_wrapped(|ui| {
                ui.label("Host:");
                egui::ComboBox::from_id_salt("host_select")
                    .selected_text(self.selected_host.display_name())
                    .width(82.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_host, HostKind::Wasapi, "WASAPI");
                        #[cfg(feature = "asio")]
                        ui.selectable_value(&mut self.selected_host, HostKind::Asio, "ASIO");
                    });

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

            if self.selected_host != prev_host {
                self.selected_input = None;
                self.selected_output = None;
                self.selected_in_channel = 0;
                self.selected_proc_rate = None;
                self.resolution_warning = None;
                self.select_defaults_if_missing();
                self.start_engine();
                return;
            }

            let in_devs = self.input_devices_with_rates();
            let in_rate = self.selected_input_native_rate();
            let in_label = format_dev_with_rate(
                self.selected_input.as_deref().unwrap_or("(sin entrada)"),
                in_rate,
                20,
            );
            let max_ch = self.max_input_channels();
            if self.selected_in_channel >= max_ch {
                self.selected_in_channel = 0;
            }
            let combo_w = (ui.available_width() - 125.0).clamp(120.0, 260.0);

            ui.horizontal_wrapped(|ui| {
                ui.label("Disp:");
                egui::ComboBox::from_id_salt("in_dev_select")
                    .selected_text(in_label)
                    .width(combo_w)
                    .show_ui(ui, |ui| {
                        for (name, rate) in &in_devs {
                            let label = format_dev_with_rate(name, *rate, 36);
                            ui.selectable_value(
                                &mut self.selected_input,
                                Some(name.clone()),
                                label,
                            );
                        }
                    });

                egui::ComboBox::from_id_salt("in_ch_select")
                    .selected_text(format!("Ch {}", self.selected_in_channel))
                    .width(62.0)
                    .show_ui(ui, |ui| {
                        for ch in 0..max_ch {
                            ui.selectable_value(
                                &mut self.selected_in_channel,
                                ch,
                                format!("Canal {ch}"),
                            );
                        }
                    });
            });

            let changed_input = self.selected_input != prev_in;
            if changed_input {
                if let (Some(req_sr), Some(new_in_sr)) =
                    (self.selected_proc_rate, self.selected_input_native_rate())
                {
                    if req_sr > new_in_sr {
                        self.selected_proc_rate = None;
                        self.resolution_warning = Some(format!(
                            "Aviso: la resolución DSP bajó a nativa ({new_in_sr} Hz) porque la nueva entrada no alcanza {req_sr} Hz."
                        ));
                    }
                }
            }

            if changed_input
                || self.selected_in_channel != prev_ch
                || self.selected_buffer != prev_buf
            {
                self.start_engine();
            }

            ui.add_space(6.0);
            let gate_th = self.active_gate_threshold_db();
            draw_level_meter(ui, "IN", self.smooth_in_db, gate_th, false);
        });
    }

    fn draw_engine_rack_block(&mut self, ui: &mut egui::Ui) {
        solid_rack_frame(Color32::from_rgb(100, 116, 139)).show(ui, |ui| {
            ui.set_min_height(132.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("⚙ MOTOR Y RESOLUCIÓN DSP")
                        .strong()
                        .size(13.5)
                        .color(Color32::from_rgb(203, 213, 225)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.engine.is_some() {
                        ui.colored_label(Color32::from_rgb(74, 222, 128), "● EN LÍNEA");
                    } else {
                        ui.colored_label(Color32::from_rgb(148, 163, 184), "○ DETENIDO");
                    }
                });
            });
            ui.add_space(4.0);

            ui.horizontal_wrapped(|ui| {
                let running = self.engine.is_some();
                let start_label = if running { "🔄 Reconectar" } else { "▶ Iniciar" };
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(start_label)
                                .strong()
                                .color(Color32::WHITE),
                        )
                        .min_size(Vec2::new(108.0, 28.0))
                        .fill(Color32::from_rgb(22, 115, 72)),
                    )
                    .clicked()
                {
                    self.start_engine();
                }

                if ui
                    .add_enabled(
                        running,
                        egui::Button::new("⏹ Detener")
                            .min_size(Vec2::new(86.0, 28.0))
                            .fill(Color32::from_rgb(95, 42, 48)),
                    )
                    .clicked()
                {
                    self.stop_engine();
                }

                if ui
                    .add(egui::Button::new("🔍 Refrescar").min_size(Vec2::new(90.0, 28.0)))
                    .clicked()
                {
                    self.refresh_devices();
                }
            });

            ui.add_space(4.0);

            // Selector de Resolución de Procesado DSP (nunca puede superar a la entrada original)
            let in_native_sr = self.selected_input_native_rate();
            let mut candidate_rate = self.selected_proc_rate;
            let rate_label = match (self.selected_proc_rate, in_native_sr) {
                (None, Some(sr)) => format!("Nativa ({sr} Hz)"),
                (None, None) => "Nativa (Auto)".to_string(),
                (Some(sr), _) => format!("{sr} Hz"),
            };

            ui.horizontal(|ui| {
                ui.label("Resolución DSP:");
                egui::ComboBox::from_id_salt("proc_rate_select")
                    .selected_text(rate_label)
                    .width(155.0)
                    .show_ui(ui, |ui| {
                        for opt in PROC_RATE_OPTIONS {
                            let label = match (opt, in_native_sr) {
                                (None, Some(sr)) => format!("Nativa de entrada ({sr} Hz)"),
                                (None, None) => "Nativa de entrada (Auto)".to_string(),
                                (Some(sr), Some(max_sr)) if sr > max_sr => {
                                    format!("{sr} Hz (bloqueado: > {max_sr} Hz)")
                                }
                                (Some(sr), _) => format!("{sr} Hz"),
                            };
                            ui.selectable_value(&mut candidate_rate, opt, label);
                        }
                    });
            });

            if candidate_rate != self.selected_proc_rate {
                if let (Some(req_sr), Some(max_sr)) = (candidate_rate, in_native_sr) {
                    if req_sr > max_sr {
                        self.resolution_warning = Some(format!(
                            "⚠ No permitido: {req_sr} Hz supera la resolución original de entrada ({max_sr} Hz). Cambia el muestreo en Windows si quieres subirlo."
                        ));
                    } else {
                        self.resolution_warning = None;
                        self.selected_proc_rate = candidate_rate;
                        self.start_engine();
                    }
                } else {
                    self.resolution_warning = None;
                    self.selected_proc_rate = candidate_rate;
                    self.start_engine();
                }
            }

            ui.add_space(2.0);

            if let Some(err) = &self.last_error {
                ui.colored_label(
                    Color32::from_rgb(248, 113, 113),
                    truncate_label(&format!("Error: {err}"), 70),
                );
            } else if let Some(eng) = &self.engine {
                let info = eng.info();
                let xruns = self.last_snapshot.underruns + self.last_snapshot.overflows;
                if !info.is_resampling_input && !info.is_resampling_output {
                    ui.colored_label(
                        Color32::from_rgb(74, 222, 128),
                        format!("✓ {} Hz nativa 1:1 (SIN RESAMPLEO)", info.processing_sample_rate),
                    );
                } else if info.is_resampling_output && !info.is_resampling_input {
                    ui.colored_label(
                        Color32::from_rgb(251, 191, 36),
                        format!(
                            "DSP: {} Hz → Salida: {} Hz (resampleo al final)",
                            info.processing_sample_rate, info.output_sample_rate
                        ),
                    );
                } else {
                    ui.colored_label(
                        Color32::from_rgb(251, 191, 36),
                        format!(
                            "In: {} Hz → DSP: {} Hz → Out: {} Hz",
                            info.input_sample_rate,
                            info.processing_sample_rate,
                            info.output_sample_rate
                        ),
                    );
                }
                let in_b = info
                    .input_buffer_frames
                    .map_or("?".to_string(), |v| v.to_string());
                let out_b = info
                    .output_buffer_frames
                    .map_or("?".to_string(), |v| v.to_string());
                ui.label(
                    egui::RichText::new(format!(
                        "Latencia: {:.1} ms  ·  Buf: {in_b}/{out_b}  ·  xruns: {xruns}",
                        info.estimated_latency_ms,
                    ))
                    .size(11.5)
                    .color(Color32::from_rgb(148, 163, 184)),
                );
            } else {
                ui.colored_label(
                    Color32::from_rgb(148, 163, 184),
                    "Selecciona dispositivos y presiona Iniciar.",
                );
            }
        });
    }

    fn draw_master_output_rack_block(&mut self, ui: &mut egui::Ui) {
        solid_rack_frame(Color32::from_rgb(34, 197, 94)).show(ui, |ui| {
            ui.set_min_height(132.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("🔊 MASTER Y SALIDA")
                        .strong()
                        .size(13.5)
                        .color(Color32::from_rgb(74, 222, 128)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    rack_fixed_badge(ui, "FIJO");
                });
            });
            ui.add_space(4.0);

            let out_devs = self.output_devices_with_rates();
            let out_rate = self.selected_output_native_rate();
            let prev_out = self.selected_output.clone();

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
            let out_label = format_dev_with_rate(
                self.selected_output.as_deref().unwrap_or("(sin salida)"),
                out_rate,
                19,
            );

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    let left_w = (ui.available_width() - 88.0).max(120.0);
                    ui.set_max_width(left_w);

                    let combo_w = (left_w - 48.0).clamp(90.0, 250.0);
                    ui.horizontal(|ui| {
                        ui.label("Disp:");
                        ui.add_enabled_ui(!is_asio, |ui| {
                            egui::ComboBox::from_id_salt("out_dev_select")
                                .selected_text(out_label)
                                .width(combo_w)
                                .show_ui(ui, |ui| {
                                    for (name, rate) in &out_devs {
                                        let label = format_dev_with_rate(name, *rate, 34);
                                        ui.selectable_value(
                                            &mut self.selected_output,
                                            Some(name.clone()),
                                            label,
                                        );
                                    }
                                });
                        })
                        .response
                        .on_disabled_hover_text(
                            "En ASIO la salida usa el mismo driver que la entrada",
                        );
                    });

                    ui.add_space(4.0);

                    let muted = self.params.mute();
                    let mute_btn = egui::Button::new(
                        egui::RichText::new(if muted {
                            "🔇 MUTE ACTIVO"
                        } else {
                            "🔊 Salida Activa"
                        })
                        .strong()
                        .color(Color32::WHITE),
                    )
                    .min_size(Vec2::new(124.0, 30.0))
                    .fill(if muted {
                        Color32::from_rgb(185, 45, 45)
                    } else {
                        Color32::from_rgb(45, 58, 76)
                    });
                    if ui.add(mute_btn).clicked() {
                        self.params.set_mute(!muted);
                    }

                    ui.add_space(6.0);
                    draw_level_meter(ui, "OUT", self.smooth_out_db, None, muted);
                });

                if let Some(master) = self.params.master_slot() {
                    if let Some(spec) = master.specs.first() {
                        let mut val = master.get(0);
                        let muted = self.params.mute();
                        let accent = Color32::from_rgb(
                            master.accent_color[0],
                            master.accent_color[1],
                            master.accent_color[2],
                        );
                        if draw_rotary_knob(
                            ui,
                            spec,
                            &mut val,
                            accent,
                            !muted && !master.bypass(),
                            0.86,
                        ) {
                            master.set(0, val);
                        }
                    }
                }
            });

            if self.selected_output != prev_out {
                self.start_engine();
            }
        });
    }

    fn draw_cascade_board(&mut self, ui: &mut egui::Ui) {
        let central_frame = Frame::new()
            .fill(Color32::from_rgb(13, 16, 21))
            .inner_margin(Margin::symmetric(14, 10));

        egui::CentralPanel::default()
            .frame(central_frame)
            .show(ui, |ui| {
                let mut mounted_positions: Vec<usize> = Vec::new();
                for pos in 0..self.params.slot_count() {
                    let slot_idx = self.params.order_at(pos);
                    if let Some(slot) = self.params.slots().get(slot_idx) {
                        if !slot.is_master && slot.is_mounted() {
                            mounted_positions.push(pos);
                        }
                    }
                }
                let kinds = self.params.kinds();
                let total_mounted = mounted_positions.len();

                // Barra de herramientas fija arriba de la zona desplazable
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("PEDALERA EN CASCADA")
                            .strong()
                            .size(15.0)
                            .color(Color32::from_rgb(226, 232, 240)),
                    );

                    ui.label(
                        egui::RichText::new(format!("({} en cadena)", total_mounted))
                            .size(13.0)
                            .color(Color32::from_rgb(148, 163, 184)),
                    );

                    ui.add_space(6.0);

                    ui.menu_button(
                        egui::RichText::new("➕ Añadir / Quitar Pedales ▾")
                            .strong()
                            .size(13.5),
                        |ui| {
                            ui.set_min_width(250.0);
                            ui.label(
                                egui::RichText::new("Añadir pedal a la cadena")
                                    .strong()
                                    .color(Color32::from_rgb(180, 192, 210)),
                            );
                            ui.separator();

                            for info in &kinds {
                                let [r, g, b] = info.accent_color;
                                let can_add =
                                    info.used < info.max && total_mounted < MAX_MOUNTED_PEDALS;
                                let btn_text = egui::RichText::new(format!(
                                    "➕ {}  ({}/{})",
                                    info.name, info.used, info.max
                                ))
                                .color(Color32::from_rgb(r, g, b));
                                let resp = ui
                                    .add_enabled(can_add, egui::Button::new(btn_text))
                                    .on_disabled_hover_text(if info.used >= info.max {
                                        "Máximo alcanzado"
                                    } else {
                                        "Límite máximo de pedales en cadena alcanzado"
                                    });
                                if resp.clicked() {
                                    self.params.mount_new_instance(info.kind);
                                }
                            }

                            if !mounted_positions.is_empty() {
                                ui.add_space(4.0);
                                ui.separator();
                                ui.label(
                                    egui::RichText::new("En pedalera")
                                        .strong()
                                        .color(Color32::from_rgb(180, 192, 210)),
                                );
                                for &cascade_pos in &mounted_positions {
                                    let slot_idx = self.params.order_at(cascade_pos);
                                    if let Some(slot) = self.params.slots().get(slot_idx) {
                                        let [r, g, b] = slot.accent_color;
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(slot.display_name())
                                                    .color(Color32::from_rgb(r, g, b)),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ui
                                                        .button("✕")
                                                        .on_hover_text("Quitar de la pedalera")
                                                        .clicked()
                                                    {
                                                        slot.set_mounted(false);
                                                    }
                                                },
                                            );
                                        });
                                    }
                                }
                            }
                        },
                    );

                    for info in &kinds {
                        if info.used == 0 {
                            let [r, g, b] = info.accent_color;
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new(format!("+ {}", info.name))
                                            .strong()
                                            .color(Color32::from_rgb(r, g, b)),
                                    )
                                    .min_size(Vec2::new(110.0, 28.0)),
                                )
                                .on_hover_text(format!(
                                    "Añadir {} al final de la cascada",
                                    info.name
                                ))
                                .clicked()
                            {
                                self.params.mount_new_instance(info.kind);
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(
                                "Arrastra perillas ↑↓ o rueda · Shift = fino · Doble clic = reset",
                            )
                            .size(12.0)
                            .color(Color32::from_rgb(120, 134, 152)),
                        );
                    });
                });

                ui.add_space(8.0);

                // Scroll bidireccional (vertical + horizontal) para que nunca se corte al achicar la ventana
                let active_proc_sr = self
                    .engine
                    .as_ref()
                    .map(|e| e.info().processing_sample_rate)
                    .or(self.selected_proc_rate)
                    .or_else(|| self.selected_input_native_rate())
                    .unwrap_or(48_000);

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
                let master_db = self.params.master_slot().map_or(-6.0, |m| m.get(0));
                let sample_rate_label = self.engine.as_ref().map(|e| {
                    let i = e.info();
                    if i.is_resampling_input || i.is_resampling_output {
                        format!("{}→{} Hz", i.processing_sample_rate, i.output_sample_rate)
                    } else {
                        format!("{} Hz", i.processing_sample_rate)
                    }
                });

                // Altura ajustada al contenido (sin espacio vacío innecesario)
                let max_knobs = mounted_positions
                    .iter()
                    .filter_map(|&pos| self.params.slots().get(self.params.order_at(pos)))
                    .map(|s| s.param_count())
                    .max()
                    .unwrap_or(1);
                let max_knob_rows = max_knobs.div_ceil(3).max(1);
                let pedal_height = 152.0 + max_knob_rows as f32 * 94.0;
                let pedal_width = 250.0;

                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;

                            draw_fixed_terminal(
                                ui,
                                "🎸 IN",
                                &truncate_label(&in_label, 14),
                                &format!(
                                    "Canal {in_ch}{}",
                                    sample_rate_label
                                        .as_deref()
                                        .map_or(String::new(), |s| format!("\n{s}"))
                                ),
                                Color32::from_rgb(59, 130, 246),
                                pedal_height,
                                active,
                            );

                            draw_signal_arrow(ui, pedal_height, active);

                            if mounted_positions.is_empty() {
                                Frame::new()
                                    .fill(Color32::from_rgb(20, 24, 31))
                                    .corner_radius(CornerRadius::same(10))
                                    .stroke(Stroke::new(1.5, Color32::from_rgb(48, 56, 70)))
                                    .inner_margin(Margin::same(16))
                                    .show(ui, |ui| {
                                        ui.set_min_size(Vec2::new(280.0, pedal_height));
                                        ui.vertical_centered(|ui| {
                                            ui.add_space(pedal_height * 0.3);
                                            ui.label(
                                                egui::RichText::new("Sin pedales en la cadena")
                                                    .strong()
                                                    .size(15.5)
                                                    .color(Color32::from_rgb(180, 192, 208)),
                                            );
                                            ui.add_space(6.0);
                                            ui.label(
                                                egui::RichText::new(
                                                    "Señal directa hacia Master Volume.",
                                                )
                                                .color(Color32::from_rgb(125, 138, 156)),
                                            );
                                        });
                                    });
                                draw_signal_arrow(ui, pedal_height, active && !muted);
                            } else {
                                let mounted_len = mounted_positions.len();
                                for (m_idx, &cascade_pos) in mounted_positions.iter().enumerate() {
                                    let slot_idx = self.params.order_at(cascade_pos);
                                    if let Some(slot) = self.params.slots().get(slot_idx) {
                                        let prev_pos = if m_idx > 0 {
                                            Some(mounted_positions[m_idx - 1])
                                        } else {
                                            None
                                        };
                                        let next_pos = if m_idx + 1 < mounted_len {
                                            Some(mounted_positions[m_idx + 1])
                                        } else {
                                            None
                                        };
                                        draw_stompbox_pedal(
                                            ui,
                                            &self.params,
                                            m_idx,
                                            cascade_pos,
                                            prev_pos,
                                            next_pos,
                                            slot,
                                            pedal_width,
                                            pedal_height,
                                        );
                                    }
                                    draw_signal_arrow(ui, pedal_height, active && !muted);
                                }
                            }

                            draw_fixed_terminal(
                                ui,
                                if muted { "🔇 MUTE" } else { "🔊 MASTER" },
                                &truncate_label(&out_label, 14),
                                &if muted {
                                    "Silenciada".to_string()
                                } else {
                                    format!("Vol: {master_db:.1} dB")
                                },
                                if muted {
                                    Color32::from_rgb(220, 38, 38)
                                } else {
                                    Color32::from_rgb(34, 197, 94)
                                },
                                pedal_height,
                                active && !muted,
                            );
                        });

                        ui.add_space(12.0);
                        self.analyzer.draw(ui, active_proc_sr);
                    });
            });
    }
}

impl eframe::App for PedalboardApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.update_meters(ui.ctx());
        self.draw_preset_bar(ui);
        self.draw_studio_rack(ui);
        self.draw_cascade_board(ui);
        self.draw_preset_dialogs(ui);
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }
}

fn load_preset_from_ref(
    pref: &PresetRef,
    user_presets: &[(String, PathBuf)],
) -> anyhow::Result<Preset> {
    if pref.source == "factory" {
        return load_factory_preset(&pref.name);
    }
    if let Some((_, path)) = user_presets
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&pref.name))
    {
        let raw = std::fs::read_to_string(path)?;
        return Preset::from_json(&raw);
    }
    resolve_preset(&pref.name)
}

fn solid_rack_frame(accent: Color32) -> Frame {
    Frame::new()
        .fill(Color32::from_rgb(23, 28, 37))
        .corner_radius(CornerRadius::same(8))
        .stroke(Stroke::new(
            1.2,
            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 110),
        ))
        .inner_margin(Margin::same(10))
}

fn rack_fixed_badge(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgb(35, 42, 54))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(10.5)
                    .strong()
                    .color(Color32::from_rgb(165, 178, 196)),
            );
        });
}

fn format_dev_with_rate(name: &str, rate: Option<u32>, max_name_chars: usize) -> String {
    let short = truncate_label(name, max_name_chars);
    match rate {
        Some(sr) if sr > 0 => format!("{short} [{sr} Hz]"),
        _ => short,
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

fn draw_level_meter(ui: &mut egui::Ui, title: &str, db: f32, marker_db: Option<f32>, muted: bool) {
    const MIN_DB: f32 = -60.0;
    const MAX_DB: f32 = 0.0;

    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .size(11.5)
                .strong()
                .color(Color32::from_rgb(175, 185, 200)),
        );

        let desired = Vec2::new((ui.available_width() - 78.0).max(80.0), 15.0);
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

fn draw_fixed_terminal(
    ui: &mut egui::Ui,
    title: &str,
    device_name: &str,
    detail: &str,
    accent: Color32,
    height: f32,
    active: bool,
) {
    let border = if active {
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 160)
    } else {
        Color32::from_rgb(52, 58, 70)
    };
    Frame::new()
        .fill(Color32::from_rgb(18, 21, 27))
        .corner_radius(CornerRadius::same(8))
        .stroke(Stroke::new(1.5, border))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(96.0, height));
            ui.set_max_size(Vec2::new(108.0, height));
            ui.vertical_centered(|ui| {
                rack_fixed_badge(ui, "CHASSIS");
                ui.add_space((height * 0.18).max(16.0));
                ui.label(egui::RichText::new(title).strong().size(13.5).color(accent));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(device_name)
                        .size(11.5)
                        .color(Color32::from_rgb(210, 218, 230)),
                );
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(detail)
                        .size(11.0)
                        .color(Color32::from_rgb(138, 150, 168)),
                );
            });
        });
}

fn draw_signal_arrow(ui: &mut egui::Ui, height: f32, active: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(20.0, height), Sense::hover());
    let color = if active {
        Color32::from_rgb(148, 163, 184)
    } else {
        Color32::from_rgb(65, 74, 88)
    };
    let y = rect.center().y;
    let p = ui.painter();
    p.line_segment(
        [
            Pos2::new(rect.min.x + 1.0, y),
            Pos2::new(rect.max.x - 3.0, y),
        ],
        Stroke::new(2.5, color),
    );
    p.line_segment(
        [
            Pos2::new(rect.max.x - 9.0, y - 5.0),
            Pos2::new(rect.max.x - 2.0, y),
        ],
        Stroke::new(2.5, color),
    );
    p.line_segment(
        [
            Pos2::new(rect.max.x - 9.0, y + 5.0),
            Pos2::new(rect.max.x - 2.0, y),
        ],
        Stroke::new(2.5, color),
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_stompbox_pedal(
    ui: &mut egui::Ui,
    params: &Params,
    mounted_idx: usize,
    cascade_pos: usize,
    prev_pos: Option<usize>,
    next_pos: Option<usize>,
    slot: &PedalParamSlot,
    pedal_width: f32,
    pedal_height: f32,
) {
    let [r, g, b] = slot.accent_color;
    let accent = Color32::from_rgb(r, g, b);
    let bypass = slot.bypass();
    let active = !bypass;

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
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_size(Vec2::new(pedal_width, pedal_height));
            ui.set_max_size(Vec2::new(pedal_width, pedal_height));

            // Cabecera del pedal: posición, reordenar, LED y botón Quitar (✕)
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("#{}", mounted_idx + 1))
                        .size(13.0)
                        .strong()
                        .color(Color32::from_rgb(160, 174, 192)),
                );

                if ui
                    .add_enabled(
                        prev_pos.is_some(),
                        egui::Button::new("◀").min_size(Vec2::new(28.0, 26.0)),
                    )
                    .on_hover_text("Mover pedal antes en la cascada")
                    .clicked()
                {
                    if let Some(p) = prev_pos {
                        params.swap_cascade_positions(cascade_pos, p);
                    }
                }

                if ui
                    .add_enabled(
                        next_pos.is_some(),
                        egui::Button::new("▶").min_size(Vec2::new(28.0, 26.0)),
                    )
                    .on_hover_text("Mover pedal después en la cascada")
                    .clicked()
                {
                    if let Some(n) = next_pos {
                        params.swap_cascade_positions(cascade_pos, n);
                    }
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("✕")
                                    .strong()
                                    .color(Color32::from_rgb(220, 140, 140)),
                            )
                            .min_size(Vec2::new(28.0, 26.0))
                            .fill(Color32::from_rgb(48, 36, 42)),
                        )
                        .on_hover_text("Quitar este pedal de la pedalera")
                        .clicked()
                    {
                        slot.set_mounted(false);
                    }

                    let (led_rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                    let c = led_rect.center();
                    let p = ui.painter();
                    if active {
                        p.circle_filled(c, 8.0, Color32::from_rgba_unmultiplied(r, g, b, 65));
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

            ui.add_space(2.0);
            ui.vertical_centered(|ui| {
                let title = slot.display_name().to_uppercase();
                let resp = ui
                    .add(
                        egui::Label::new(egui::RichText::new(title).strong().size(15.0).color(
                            if active {
                                Color32::from_rgb(241, 245, 249)
                            } else {
                                Color32::from_rgb(125, 134, 148)
                            },
                        ))
                        .sense(Sense::click()),
                    )
                    .on_hover_text("Clic derecho para renombrar");
                resp.context_menu(|ui| {
                    let mut label_buf = slot.label();
                    ui.label(egui::RichText::new("Etiqueta del pedal").strong());
                    if ui.text_edit_singleline(&mut label_buf).changed() {
                        slot.set_label(&label_buf);
                    }
                    if ui.button("Restablecer nombre").clicked() {
                        slot.set_label("");
                        ui.close();
                    }
                });
            });

            // Zona compacta de perillas: hasta 3 por fila para evitar alturas innecesarias
            let knob_count = slot.param_count();
            let knob_scale = if knob_count <= 2 { 0.92 } else { 0.80 };
            let knob_w = 88.0 * knob_scale;
            let knob_h = 104.0 * knob_scale;
            let row_count = knob_count.div_ceil(3).max(1);
            let row_gap = 4.0;
            let knobs_total_h = row_count as f32 * (knob_h + row_gap);
            let footer_reserve = 72.0;
            let content_h = (pedal_height - 20.0 - 50.0).max(150.0);
            let top_pad = ((content_h - knobs_total_h - footer_reserve) * 0.4).max(4.0);

            ui.add_space(top_pad);

            let indices: Vec<usize> = (0..knob_count).collect();
            for row in indices.chunks(3) {
                let item_gap = if row.len() == 3 { 6.0 } else { 14.0 };
                let row_w =
                    row.len() as f32 * knob_w + (row.len().saturating_sub(1) as f32) * item_gap;
                let side_pad = ((pedal_width - 20.0 - row_w) * 0.5).max(0.0);

                ui.horizontal(|ui| {
                    if side_pad > 0.0 {
                        ui.add_space(side_pad);
                    }
                    ui.spacing_mut().item_spacing.x = item_gap;
                    for &p_idx in row {
                        let spec = &slot.specs[p_idx];
                        let mut val = slot.get(p_idx);
                        if draw_rotary_knob(ui, spec, &mut val, accent, active, knob_scale) {
                            slot.set(p_idx, val);
                        }
                    }
                });
                ui.add_space(row_gap);
            }

            let bottom_pad = (content_h - top_pad - knobs_total_h - footer_reserve).max(4.0);
            ui.add_space(bottom_pad);

            // Zona inferior compacta: Footswitch + Reset alineados
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
                        .size(13.5)
                        .color(Color32::WHITE),
                )
                .min_size(Vec2::new(146.0, 34.0))
                .corner_radius(CornerRadius::same(17))
                .fill(switch_fill);

                if ui.add(switch_btn).clicked() {
                    slot.set_bypass(!bypass);
                }

                ui.add_space(4.0);
                if ui
                    .add(egui::Button::new("↺ Reset").min_size(Vec2::new(78.0, 24.0)))
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
    scale: f32,
) -> bool {
    let desired_size = Vec2::new(88.0 * scale, 104.0 * scale);
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
        let center = Pos2::new(rect.center().x, rect.min.y + 52.0 * scale);
        let outer_radius = 27.0 * scale;
        let body_radius = 20.0 * scale;

        let title_color = if enabled {
            Color32::from_rgb(203, 213, 225)
        } else {
            Color32::from_rgb(115, 125, 140)
        };
        p.text(
            Pos2::new(rect.center().x, rect.min.y + 11.0 * scale),
            Align2::CENTER_CENTER,
            spec.name,
            FontId::proportional((13.0 * scale).max(11.0)),
            title_color,
        );

        let start_angle = -0.75 * std::f32::consts::PI;
        let sweep = 1.5 * std::f32::consts::PI;
        let active_angle = start_angle + norm * sweep;

        let bg_points: Vec<Pos2> = (0..=28)
            .map(|i| {
                let t = i as f32 / 28.0;
                let a = start_angle + t * sweep;
                center + Vec2::new(a.sin(), -a.cos()) * outer_radius
            })
            .collect();
        p.line(
            bg_points,
            Stroke::new(3.5 * scale.min(1.15), Color32::from_rgb(48, 55, 68)),
        );

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
            p.line(active_points, Stroke::new(3.5 * scale.min(1.15), arc_color));
        }

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

        let dir = Vec2::new(active_angle.sin(), -active_angle.cos());
        let pointer_color = if enabled {
            Color32::WHITE
        } else {
            Color32::from_rgb(140, 148, 160)
        };
        p.line_segment(
            [
                center + dir * (5.0 * scale),
                center + dir * (body_radius - 3.0),
            ],
            Stroke::new(2.5 * scale.min(1.1), pointer_color),
        );

        p.text(
            Pos2::new(rect.center().x, rect.max.y - 12.0 * scale),
            Align2::CENTER_CENTER,
            spec.format_value(*value),
            FontId::monospace((12.0 * scale).max(10.5)),
            if enabled {
                Color32::from_rgb(226, 232, 240)
            } else {
                Color32::from_rgb(120, 130, 145)
            },
        );
    }

    changed
}
