use std::collections::VecDeque;

use dsp::{compute_spectrum_db, compute_waveform_metrics, find_rising_zero_crossing};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CornerRadius, FontId, Frame, Margin, Pos2, Rect, Sense,
    Stroke, StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use engine::Engine;

const ANALYZER_BUF_LEN: usize = 16_384;
const FFT_LEN: usize = 4096;
const FFT_BINS: usize = FFT_LEN / 2;
const WATERFALL_W: usize = 256;
const WATERFALL_H: usize = 96;
const MIN_FREQ_HZ: f32 = 40.0;
const MAX_FREQ_HZ: f32 = 16_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceView {
    Both,
    PreOnly,
    PostOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaterfallSource {
    Post,
    Pre,
}

pub struct AnalyzerState {
    pub visible: bool,
    pub frozen: bool,
    pub relative_ref: bool,
    pub view_mode: TraceView,
    pub waterfall_source: WaterfallSource,
    pub time_window_ms: f32,
    pub abs_zoom: f32,

    pre_ring: VecDeque<f32>,
    post_ring: VecDeque<f32>,
    pre_scratch: Vec<f32>,
    post_scratch: Vec<f32>,

    pre_spec_db: Vec<f32>,
    post_spec_db: Vec<f32>,
    smooth_pre_spec: Vec<f32>,
    smooth_post_spec: Vec<f32>,

    waterfall_pixels: Vec<Color32>,
    waterfall_tex: Option<TextureHandle>,
    last_sr: u32,
}

impl Default for AnalyzerState {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalyzerState {
    pub fn new() -> Self {
        let mut pre_ring = VecDeque::with_capacity(ANALYZER_BUF_LEN);
        let mut post_ring = VecDeque::with_capacity(ANALYZER_BUF_LEN);
        pre_ring.resize(ANALYZER_BUF_LEN, 0.0);
        post_ring.resize(ANALYZER_BUF_LEN, 0.0);

        Self {
            visible: true,
            frozen: false,
            relative_ref: true,
            view_mode: TraceView::Both,
            waterfall_source: WaterfallSource::Post,
            time_window_ms: 10.0,
            abs_zoom: 4.0,
            pre_ring,
            post_ring,
            pre_scratch: vec![0.0; ANALYZER_BUF_LEN],
            post_scratch: vec![0.0; ANALYZER_BUF_LEN],
            pre_spec_db: vec![-100.0; FFT_BINS],
            post_spec_db: vec![-100.0; FFT_BINS],
            smooth_pre_spec: vec![-100.0; FFT_BINS],
            smooth_post_spec: vec![-100.0; FFT_BINS],
            waterfall_pixels: vec![Color32::from_rgb(8, 10, 15); WATERFALL_W * WATERFALL_H],
            waterfall_tex: None,
            last_sr: 48_000,
        }
    }

    pub fn update_from_engine(
        &mut self,
        ctx: &egui::Context,
        engine: Option<&mut Engine>,
        proc_sr: u32,
    ) {
        self.last_sr = proc_sr.max(8_000);
        let mut received_any = false;

        if let Some(eng) = engine {
            let frozen = self.frozen;
            let pre_ring = &mut self.pre_ring;
            let post_ring = &mut self.post_ring;
            eng.poll_analysis(|pre, post| {
                if !frozen {
                    if pre_ring.len() >= ANALYZER_BUF_LEN {
                        pre_ring.pop_front();
                    }
                    if post_ring.len() >= ANALYZER_BUF_LEN {
                        post_ring.pop_front();
                    }
                    pre_ring.push_back(pre);
                    post_ring.push_back(post);
                    received_any = true;
                }
            });
        }

        if self.visible && !self.frozen && received_any {
            copy_deque_to_slice(&self.pre_ring, &mut self.pre_scratch);
            copy_deque_to_slice(&self.post_ring, &mut self.post_scratch);

            let start = self.pre_scratch.len().saturating_sub(FFT_LEN);
            compute_spectrum_db(&self.pre_scratch[start..], &mut self.pre_spec_db);
            compute_spectrum_db(&self.post_scratch[start..], &mut self.post_spec_db);

            for i in 0..FFT_BINS {
                let a = if self.pre_spec_db[i] > self.smooth_pre_spec[i] {
                    0.65
                } else {
                    0.25
                };
                self.smooth_pre_spec[i] += a * (self.pre_spec_db[i] - self.smooth_pre_spec[i]);

                let b = if self.post_spec_db[i] > self.smooth_post_spec[i] {
                    0.65
                } else {
                    0.25
                };
                self.smooth_post_spec[i] += b * (self.post_spec_db[i] - self.smooth_post_spec[i]);
            }

            self.push_waterfall_row(ctx);
        }
    }

    fn push_waterfall_row(&mut self, ctx: &egui::Context) {
        // Desplazar filas existentes hacia abajo
        let total = WATERFALL_W * WATERFALL_H;
        self.waterfall_pixels
            .copy_within(0..(total - WATERFALL_W), WATERFALL_W);

        let spec = match self.waterfall_source {
            WaterfallSource::Post => &self.smooth_post_spec,
            WaterfallSource::Pre => &self.smooth_pre_spec,
        };

        let ref_peak = if self.relative_ref {
            peak_in_band(spec, self.last_sr).max(-80.0)
        } else {
            0.0
        };

        let log_min = MIN_FREQ_HZ.ln();
        let log_span = (MAX_FREQ_HZ / MIN_FREQ_HZ).ln();
        let bin_hz = self.last_sr as f32 / FFT_LEN as f32;

        for x in 0..WATERFALL_W {
            let u = x as f32 / (WATERFALL_W - 1) as f32;
            let freq = (log_min + u * log_span).exp();
            let bin_f = (freq / bin_hz).clamp(1.0, (FFT_BINS - 2) as f32);
            let b0 = bin_f.floor() as usize;
            let frac = bin_f - b0 as f32;
            let db = spec[b0] * (1.0 - frac) + spec[b0 + 1] * frac;

            let norm = if self.relative_ref && ref_peak > -75.0 {
                ((db - ref_peak + 72.0) / 72.0).clamp(0.0, 1.0)
            } else {
                ((db + 90.0) / 90.0).clamp(0.0, 1.0)
            };
            self.waterfall_pixels[x] = waterfall_color(norm);
        }

        let image = ColorImage::new([WATERFALL_W, WATERFALL_H], self.waterfall_pixels.clone());
        match &mut self.waterfall_tex {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => {
                self.waterfall_tex =
                    Some(ctx.load_texture("waterfall_spec", image, TextureOptions::LINEAR));
            }
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, proc_sr: u32) {
        self.last_sr = proc_sr.max(8_000);

        Frame::new()
            .fill(Color32::from_rgb(17, 21, 28))
            .corner_radius(CornerRadius::same(10))
            .stroke(Stroke::new(1.2, Color32::from_rgb(40, 48, 62)))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                // Cabecera del analizador
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("🔬 ANALIZADOR EN TIEMPO REAL: PRE (LIMPIO) vs POST (EFECTOS)")
                            .strong()
                            .size(14.0)
                            .color(Color32::from_rgb(226, 232, 240)),
                    );

                    let toggle_lbl = if self.visible {
                        "👁 Ocultar"
                    } else {
                        "👁 Mostrar Analizador"
                    };
                    if ui
                        .add(egui::Button::new(toggle_lbl).min_size(Vec2::new(92.0, 26.0)))
                        .clicked()
                    {
                        self.visible = !self.visible;
                    }

                    if self.visible {
                        let freeze_btn = egui::Button::new(if self.frozen {
                            "▶ Reanudar"
                        } else {
                            "⏸ Congelar"
                        })
                        .min_size(Vec2::new(96.0, 26.0))
                        .fill(if self.frozen {
                            Color32::from_rgb(160, 100, 24)
                        } else {
                            Color32::from_rgb(38, 46, 58)
                        });
                        if ui.add(freeze_btn).clicked() {
                            self.frozen = !self.frozen;
                        }

                        let ref_btn = egui::Button::new(if self.relative_ref {
                            "🎯 Ref. Relativa (Auto)"
                        } else {
                            "📏 Escala Absoluta"
                        })
                        .min_size(Vec2::new(148.0, 26.0))
                        .fill(if self.relative_ref {
                            Color32::from_rgb(24, 92, 118)
                        } else {
                            Color32::from_rgb(44, 52, 66)
                        });
                        if ui
                            .add(ref_btn)
                            .on_hover_text(
                                "Normaliza PRE y POST respecto a su pico/fundamental para comparar la forma armónica sin importar la diferencia de volumen",
                            )
                            .clicked()
                        {
                            self.relative_ref = !self.relative_ref;
                        }

                        if !self.relative_ref {
                            ui.add(
                                egui::Slider::new(&mut self.abs_zoom, 1.0..=50.0)
                                    .logarithmic(true)
                                    .text("Zoom"),
                            );
                        }

                        ui.label("Trazos:");
                        for (mode, lbl) in [
                            (TraceView::Both, "PRE + POST"),
                            (TraceView::PreOnly, "Solo PRE"),
                            (TraceView::PostOnly, "Solo POST"),
                        ] {
                            ui.selectable_value(&mut self.view_mode, mode, lbl);
                        }

                        ui.label("Ventana:");
                        for ms in [5.0_f32, 10.0, 25.0] {
                            ui.selectable_value(&mut self.time_window_ms, ms, format!("{ms:.0} ms"));
                        }
                    }
                });

                if !self.visible {
                    return;
                }

                ui.add_space(8.0);

                if ui.available_width() >= 920.0 {
                    ui.columns(2, |cols| {
                        self.draw_oscilloscope(&mut cols[0]);
                        self.draw_spectrum_and_waterfall(&mut cols[1]);
                    });
                } else {
                    self.draw_oscilloscope(ui);
                    ui.add_space(10.0);
                    self.draw_spectrum_and_waterfall(ui);
                }
            });
    }

    fn draw_oscilloscope(&self, ui: &mut egui::Ui) {
        let win_samples = ((self.time_window_ms / 1_000.0) * self.last_sr as f32)
            .round()
            .clamp(64.0, (ANALYZER_BUF_LEN / 2) as f32) as usize;
        let search_len = (win_samples * 2).min(ANALYZER_BUF_LEN - win_samples);
        let tail_len = (win_samples + search_len).min(self.pre_scratch.len());
        let tail_start = self.pre_scratch.len().saturating_sub(tail_len);

        let pre_tail = &self.pre_scratch[tail_start..];
        let post_tail = &self.post_scratch[tail_start..];

        let pre_metrics = compute_waveform_metrics(&pre_tail[pre_tail.len().saturating_sub(win_samples)..]);
        let post_metrics = compute_waveform_metrics(&post_tail[post_tail.len().saturating_sub(win_samples)..]);

        let trigger_offset = match self.view_mode {
            TraceView::PreOnly => find_rising_zero_crossing(pre_tail, search_len),
            TraceView::PostOnly => find_rising_zero_crossing(post_tail, search_len),
            TraceView::Both => {
                if pre_metrics.peak > 0.002 {
                    find_rising_zero_crossing(pre_tail, search_len)
                } else {
                    find_rising_zero_crossing(post_tail, search_len)
                }
            }
        };

        let end = (trigger_offset + win_samples).min(pre_tail.len());
        let pre_slice = &pre_tail[trigger_offset..end];
        let post_slice = &post_tail[trigger_offset..end];

        let (scale_pre, scale_post) = if self.relative_ref {
            (
                0.88 / pre_metrics.peak.max(0.002),
                0.88 / post_metrics.peak.max(0.002),
            )
        } else {
            (self.abs_zoom, self.abs_zoom)
        };

        Frame::new()
            .fill(Color32::from_rgb(11, 14, 19))
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0, Color32::from_rgb(38, 45, 58)))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("〰 OSCILOSCOPIO DE ONDA")
                            .strong()
                            .size(12.5)
                            .color(Color32::from_rgb(203, 213, 225)),
                    );
                    ui.label(
                        egui::RichText::new("■ PRE (Limpio)")
                            .size(11.5)
                            .color(Color32::from_rgb(56, 189, 248)),
                    );
                    ui.label(
                        egui::RichText::new("■ POST (Efectos)")
                            .size(11.5)
                            .color(Color32::from_rgb(251, 146, 60)),
                    );
                });

                // Métricas matemáticas (Cresta ~3.01 dB = senoidal, <1.8 dB = cuadrada/recortada)
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "PRE: {:.1} dBFS · Cresta {:.2} dB (x{:.1})",
                            pre_metrics.peak_db, pre_metrics.crest_factor_db, scale_pre
                        ))
                        .monospace()
                        .size(11.0)
                        .color(Color32::from_rgb(125, 211, 252)),
                    );
                    ui.separator();
                    let delta_db = post_metrics.peak_db - pre_metrics.peak_db;
                    ui.label(
                        egui::RichText::new(format!(
                            "POST: {:.1} dBFS · Cresta {:.2} dB (x{:.1}) · Δ {:+.1} dB",
                            post_metrics.peak_db,
                            post_metrics.crest_factor_db,
                            scale_post,
                            delta_db
                        ))
                        .monospace()
                        .size(11.0)
                        .color(Color32::from_rgb(253, 186, 116)),
                    );
                });

                ui.add_space(4.0);

                let plot_w = ui.available_width().max(240.0);
                let (rect, _) = ui.allocate_exact_size(Vec2::new(plot_w, 232.0), Sense::hover());
                let p = ui.painter();

                p.rect(
                    rect,
                    CornerRadius::same(6),
                    Color32::from_rgb(8, 10, 14),
                    Stroke::new(1.0, Color32::from_rgb(35, 42, 54)),
                    StrokeKind::Inside,
                );

                // Rejilla de amplitud y tiempo
                for div in 1..4 {
                    let y = rect.min.y + rect.height() * (div as f32 / 4.0);
                    let stroke = if div == 2 {
                        Stroke::new(1.0, Color32::from_rgb(56, 65, 82))
                    } else {
                        Stroke::new(0.8, Color32::from_rgb(26, 31, 40))
                    };
                    p.line_segment([Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)], stroke);
                }
                for div in 1..5 {
                    let x = rect.min.x + rect.width() * (div as f32 / 5.0);
                    p.line_segment(
                        [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                        Stroke::new(0.8, Color32::from_rgb(26, 31, 40)),
                    );
                }

                let paint_trace = |slice: &[f32], scale: f32, color: Color32, width: f32| {
                    if slice.len() < 2 {
                        return;
                    }
                    let max_pts = (rect.width() as usize).clamp(64, 512);
                    let step = (slice.len() as f32 / max_pts as f32).max(1.0);
                    let mut pts = Vec::with_capacity(max_pts + 1);
                    let cy = rect.center().y;
                    let half_h = rect.height() * 0.46;

                    let mut idx_f = 0.0_f32;
                    while (idx_f as usize) < slice.len() {
                        let i = idx_f as usize;
                        let t = i as f32 / (slice.len() - 1) as f32;
                        let x = rect.min.x + t * rect.width();
                        let y_norm = (slice[i] * scale).clamp(-1.05, 1.05);
                        let y = cy - y_norm * half_h;
                        pts.push(Pos2::new(x, y));
                        idx_f += step;
                    }
                    p.line(pts, Stroke::new(width, color));
                };

                if matches!(self.view_mode, TraceView::Both | TraceView::PreOnly) {
                    paint_trace(
                        pre_slice,
                        scale_pre,
                        Color32::from_rgb(56, 189, 248),
                        1.8,
                    );
                }
                if matches!(self.view_mode, TraceView::Both | TraceView::PostOnly) {
                    paint_trace(
                        post_slice,
                        scale_post,
                        Color32::from_rgb(251, 146, 60),
                        2.0,
                    );
                }

                p.text(
                    Pos2::new(rect.max.x - 6.0, rect.max.y - 6.0),
                    Align2::RIGHT_BOTTOM,
                    "Senoidal ≈ 3.0 dB cresta · Cuadrada ≈ 0.0 dB",
                    FontId::proportional(10.5),
                    Color32::from_rgb(100, 114, 134),
                );
            });
    }

    fn draw_spectrum_and_waterfall(&mut self, ui: &mut egui::Ui) {
        Frame::new()
            .fill(Color32::from_rgb(11, 14, 19))
            .corner_radius(CornerRadius::same(8))
            .stroke(Stroke::new(1.0, Color32::from_rgb(38, 45, 58)))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("📊 ESPECTRO ARMÓNICO (FFT) Y ESPECTROGRAMA")
                            .strong()
                            .size(12.5)
                            .color(Color32::from_rgb(203, 213, 225)),
                    );
                    ui.label("Cascada:");
                    ui.selectable_value(
                        &mut self.waterfall_source,
                        WaterfallSource::Post,
                        "POST",
                    );
                    ui.selectable_value(
                        &mut self.waterfall_source,
                        WaterfallSource::Pre,
                        "PRE",
                    );
                });

                ui.add_space(4.0);

                // Parte superior: Espectro FFT PRE vs POST (log 40 Hz .. 16 kHz)
                let plot_w = ui.available_width().max(240.0);
                let (spec_rect, _) =
                    ui.allocate_exact_size(Vec2::new(plot_w, 134.0), Sense::hover());
                let p = ui.painter();

                p.rect(
                    spec_rect,
                    CornerRadius::same(6),
                    Color32::from_rgb(8, 10, 14),
                    Stroke::new(1.0, Color32::from_rgb(35, 42, 54)),
                    StrokeKind::Inside,
                );

                let log_min = MIN_FREQ_HZ.ln();
                let log_span = (MAX_FREQ_HZ / MIN_FREQ_HZ).ln();
                let freq_to_x = |f: f32| -> f32 {
                    let u = ((f.ln() - log_min) / log_span).clamp(0.0, 1.0);
                    spec_rect.min.x + u * spec_rect.width()
                };

                // Rejilla de frecuencias
                for (f, lbl) in [
                    (60.0, "60"),
                    (125.0, "125"),
                    (250.0, "250"),
                    (500.0, "500"),
                    (1_000.0, "1k"),
                    (2_000.0, "2k"),
                    (4_000.0, "4k"),
                    (8_000.0, "8k"),
                ] {
                    let x = freq_to_x(f);
                    p.line_segment(
                        [Pos2::new(x, spec_rect.min.y), Pos2::new(x, spec_rect.max.y)],
                        Stroke::new(0.8, Color32::from_rgb(26, 31, 40)),
                    );
                    p.text(
                        Pos2::new(x + 2.0, spec_rect.max.y - 4.0),
                        Align2::LEFT_BOTTOM,
                        lbl,
                        FontId::monospace(9.5),
                        Color32::from_rgb(95, 108, 128),
                    );
                }

                // Rejilla de dB (-80 .. 0 dB / dBr)
                let db_min = -80.0_f32;
                let db_max = 3.0_f32;
                let db_to_y = |db: f32| -> f32 {
                    let n = ((db - db_min) / (db_max - db_min)).clamp(0.0, 1.0);
                    spec_rect.max.y - n * spec_rect.height()
                };
                for db_line in [0.0_f32, -20.0, -40.0, -60.0] {
                    let y = db_to_y(db_line);
                    p.line_segment(
                        [Pos2::new(spec_rect.min.x, y), Pos2::new(spec_rect.max.x, y)],
                        Stroke::new(0.8, Color32::from_rgb(28, 34, 44)),
                    );
                    let tag = if self.relative_ref {
                        format!("{db_line:.0} dBr")
                    } else {
                        format!("{db_line:.0} dB")
                    };
                    p.text(
                        Pos2::new(spec_rect.min.x + 4.0, y - 2.0),
                        Align2::LEFT_BOTTOM,
                        tag,
                        FontId::monospace(9.5),
                        Color32::from_rgb(95, 108, 128),
                    );
                }

                let pre_ref = if self.relative_ref {
                    let pk = peak_in_band(&self.smooth_pre_spec, self.last_sr);
                    if pk > -75.0 { pk } else { 0.0 }
                } else {
                    0.0
                };
                let post_ref = if self.relative_ref {
                    let pk = peak_in_band(&self.smooth_post_spec, self.last_sr);
                    if pk > -75.0 { pk } else { 0.0 }
                } else {
                    0.0
                };

                let paint_spec = |spec: &[f32], ref_db: f32, color: Color32, width: f32| {
                    let cols = (spec_rect.width() as usize).clamp(64, 320);
                    let bin_hz = self.last_sr as f32 / FFT_LEN as f32;
                    let mut pts = Vec::with_capacity(cols);
                    for i in 0..cols {
                        let u = i as f32 / (cols - 1) as f32;
                        let freq = (log_min + u * log_span).exp();
                        let bin_f = (freq / bin_hz).clamp(1.0, (FFT_BINS - 2) as f32);
                        let b0 = bin_f.floor() as usize;
                        let frac = bin_f - b0 as f32;
                        let raw_db = spec[b0] * (1.0 - frac) + spec[b0 + 1] * frac;
                        let rel_db = raw_db - ref_db;
                        pts.push(Pos2::new(spec_rect.min.x + u * spec_rect.width(), db_to_y(rel_db)));
                    }
                    p.line(pts, Stroke::new(width, color));
                };

                if matches!(self.view_mode, TraceView::Both | TraceView::PreOnly) {
                    paint_spec(
                        &self.smooth_pre_spec,
                        pre_ref,
                        Color32::from_rgb(56, 189, 248),
                        1.7,
                    );
                }
                if matches!(self.view_mode, TraceView::Both | TraceView::PostOnly) {
                    paint_spec(
                        &self.smooth_post_spec,
                        post_ref,
                        Color32::from_rgb(251, 146, 60),
                        2.0,
                    );
                }

                ui.add_space(6.0);

                // Parte inferior: Espectrograma 2D (Waterfall tiempo x frecuencia)
                let (wf_rect, _) =
                    ui.allocate_exact_size(Vec2::new(plot_w, 102.0), Sense::hover());
                let p = ui.painter();
                if let Some(tex) = &self.waterfall_tex {
                    p.image(
                        tex.id(),
                        wf_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                } else {
                    p.rect_filled(wf_rect, CornerRadius::same(4), Color32::from_rgb(8, 10, 15));
                }
                p.rect_stroke(
                    wf_rect,
                    CornerRadius::same(4),
                    Stroke::new(1.0, Color32::from_rgb(35, 42, 54)),
                    StrokeKind::Inside,
                );
                p.text(
                    Pos2::new(wf_rect.min.x + 6.0, wf_rect.min.y + 4.0),
                    Align2::LEFT_TOP,
                    format!(
                        "Espectrograma {} (40 Hz → 16 kHz · Tiempo ↓)",
                        match self.waterfall_source {
                            WaterfallSource::Post => "POST",
                            WaterfallSource::Pre => "PRE",
                        }
                    ),
                    FontId::proportional(10.5),
                    Color32::from_rgba_unmultiplied(230, 236, 245, 200),
                );
            });
    }
}

fn copy_deque_to_slice(dq: &VecDeque<f32>, dst: &mut [f32]) {
    let (s1, s2) = dq.as_slices();
    let n1 = s1.len().min(dst.len());
    dst[..n1].copy_from_slice(&s1[..n1]);
    let rem = dst.len().saturating_sub(n1);
    let n2 = s2.len().min(rem);
    if n2 > 0 {
        dst[n1..n1 + n2].copy_from_slice(&s2[..n2]);
    }
}

fn peak_in_band(spec_db: &[f32], sample_rate: u32) -> f32 {
    let bin_hz = sample_rate.max(8_000) as f32 / FFT_LEN as f32;
    let k_min = (MIN_FREQ_HZ / bin_hz).ceil() as usize;
    let k_max = ((MAX_FREQ_HZ / bin_hz).floor() as usize).min(spec_db.len().saturating_sub(1));
    let mut best = -100.0_f32;
    for &v in &spec_db[k_min.min(k_max)..=k_max] {
        if v > best {
            best = v;
        }
    }
    best
}

fn waterfall_color(norm: f32) -> Color32 {
    let t = norm.clamp(0.0, 1.0);
    if t < 0.25 {
        let u = t / 0.25;
        Color32::from_rgb(8, (10.0 + u * 28.0) as u8, (16.0 + u * 84.0) as u8)
    } else if t < 0.55 {
        let u = (t - 0.25) / 0.30;
        Color32::from_rgb(
            (8.0 + u * 30.0) as u8,
            (38.0 + u * 165.0) as u8,
            (100.0 + u * 135.0) as u8,
        )
    } else if t < 0.82 {
        let u = (t - 0.55) / 0.27;
        Color32::from_rgb(
            (38.0 + u * 212.0) as u8,
            (203.0 + u * 20.0) as u8,
            (235.0 - u * 195.0) as u8,
        )
    } else {
        let u = (t - 0.82) / 0.18;
        Color32::from_rgb(
            255,
            (223.0 - u * 155.0) as u8,
            (40.0 + u * 20.0) as u8,
        )
    }
}
