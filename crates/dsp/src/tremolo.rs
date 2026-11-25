//! Pedal **Tremolo** dual (`Optical` + `Harmonic` estilo Fender Brownface).
//!
//! - **`Optical`:** modulación suave de amplitud con onda LFO moldeable continuamente de senoidal a
//!   cuadrada suavizada mediante `Shape`.
//! - **`Harmonic`:** divisor Linkwitz-Riley de 2.º orden (`LP - HP` con `Q = 0.5` a `700 Hz` sobre
//!   [`SvfTpt`], *all-pass* unitario exacto en reposo) modulando graves y agudos en contrafase de $180^\circ$.

use std::f64::consts::TAU;

use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::svf::{SvfMode, SvfTpt};
use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const TREM_MODES: &[&str] = &["Optical", "Harmonic"];
const HARMONIC_CROSSOVER_HZ: f32 = 700.0;

const TREMOLO_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "mode",
        name: "Mode",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        scale: ParamScale::Stepped { labels: TREM_MODES },
        decimals: 0,
    },
    ParamSpec {
        id: "rate_hz",
        name: "Rate",
        unit: "Hz",
        min: 0.5,
        max: 14.0,
        default: 4.2,
        scale: ParamScale::Logarithmic,
        decimals: 2,
    },
    ParamSpec {
        id: "depth",
        name: "Depth",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 65.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "shape",
        name: "Shape",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 25.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "level_db",
        name: "Level",
        unit: "dB",
        min: -12.0,
        max: 12.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
];

/// Pedal Tremolo `Optical` / `Harmonic` con conmutación sin clics.
pub struct Tremolo {
    mode: f32,
    rate_hz: f32,
    depth: f32,
    shape: f32,
    level_db: f32,
    sample_rate: f32,

    xover_lp: SvfTpt,
    xover_hp: SvfTpt,

    mode_xfade: ModeCrossfader,
    rate_s: OnePoleSmoother,
    depth_s: OnePoleSmoother,
    shape_s: OnePoleSmoother,
    level_s: OnePoleSmoother,

    lfo_phase: f64,
}

impl Default for Tremolo {
    fn default() -> Self {
        Self::new()
    }
}

impl Tremolo {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let def_mode = TREMOLO_PARAMS[0].step_index(TREMOLO_PARAMS[0].default);
        let mut t = Self {
            mode: TREMOLO_PARAMS[0].default,
            rate_hz: TREMOLO_PARAMS[1].default,
            depth: TREMOLO_PARAMS[2].default,
            shape: TREMOLO_PARAMS[3].default,
            level_db: TREMOLO_PARAMS[4].default,
            sample_rate: sr,
            xover_lp: SvfTpt::new(sr),
            xover_hp: SvfTpt::new(sr),
            mode_xfade: ModeCrossfader::new(5.0, sr, def_mode),
            rate_s: OnePoleSmoother::new(15.0, sr, TREMOLO_PARAMS[1].default),
            depth_s: OnePoleSmoother::new(15.0, sr, TREMOLO_PARAMS[2].default * 0.01),
            shape_s: OnePoleSmoother::new(15.0, sr, TREMOLO_PARAMS[3].default * 0.01),
            level_s: OnePoleSmoother::new(10.0, sr, db_to_lin(TREMOLO_PARAMS[4].default)),
            lfo_phase: 0.0,
        };
        t.prepare(sr, 512);
        t
    }

    pub fn set_mode(&mut self, mode: f32) {
        self.set_param(0, mode);
    }

    pub fn set_rate_hz(&mut self, hz: f32) {
        self.set_param(1, hz);
    }

    pub fn set_depth(&mut self, pct: f32) {
        self.set_param(2, pct);
    }

    pub fn set_shape(&mut self, pct: f32) {
        self.set_param(3, pct);
    }

    pub fn set_level_db(&mut self, db: f32) {
        self.set_param(4, db);
    }

    #[inline]
    fn shaped_lfo(phase: f64, shape_norm: f64) -> f64 {
        let s = (TAU * phase).sin();
        let drive = 1.0 + 5.5 * shape_norm;
        (drive * s).tanh() / drive.tanh()
    }
}

impl Effect for Tremolo {
    fn kind(&self) -> &'static str {
        "tremolo"
    }

    fn name(&self) -> &'static str {
        "Tremolo"
    }

    fn accent_color(&self) -> [u8; 3] {
        [20, 184, 166]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &TREMOLO_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.mode,
            1 => self.rate_hz,
            2 => self.depth,
            3 => self.shape,
            4 => self.level_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = TREMOLO_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                self.mode = v;
                self.mode_xfade.set_mode(spec.step_index(v));
            }
            1 => {
                self.rate_hz = v;
                self.rate_s.set_target(v);
            }
            2 => {
                self.depth = v;
                self.depth_s.set_target(v * 0.01);
            }
            3 => {
                self.shape = v;
                self.shape_s.set_target(v * 0.01);
            }
            4 => {
                self.level_db = v;
                self.level_s.set_target(db_to_lin(v));
            }
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        self.xover_lp.set_sample_rate(sr);
        self.xover_hp.set_sample_rate(sr);
        let fc = HARMONIC_CROSSOVER_HZ.min(0.42 * sr);
        self.xover_lp.set_params(SvfMode::LowPass, fc, 0.5, 0.0);
        self.xover_hp.set_params(SvfMode::HighPass, fc, 0.5, 0.0);

        self.mode_xfade.set_sample_rate(sr);
        self.rate_s.set_sample_rate(sr);
        self.depth_s.set_sample_rate(sr);
        self.shape_s.set_sample_rate(sr);
        self.level_s.set_sample_rate(sr);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sr = (self.sample_rate as f64).max(1.0);

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s) as f64;
            let rate = self.rate_s.next_f64();
            let depth = self.depth_s.next_f64();
            let shape = self.shape_s.next_f64();
            let lvl = self.level_s.next_f64();

            let lfo = Self::shaped_lfo(self.lfo_phase, shape);
            self.lfo_phase = (self.lfo_phase + rate / sr).fract();

            let g_low = 1.0 - depth * 0.5 * (1.0 - lfo);
            let xfade = self.mode_xfade.is_crossfading();
            let (prev_m, next_m) = self.mode_xfade.pair();

            let wet = if xfade || next_m == 1 {
                let low = self.xover_lp.process_sample_f64(x);
                // En Linkwitz-Riley de 2.º orden (Q=0.5), LP - HP forma un all-pass exacto de 0.00 dB.
                let high = -self.xover_hp.process_sample_f64(x);
                let g_high = 1.0 - depth * 0.5 * (1.0 + lfo);
                let y_harm = low * g_low + high * g_high;
                if xfade {
                    let y_opt = x * g_low;
                    let y_p = if prev_m == 0 { y_opt } else { y_harm } as f32;
                    let y_n = if next_m == 0 { y_opt } else { y_harm } as f32;
                    self.mode_xfade.mix_sample(y_p, y_n) as f64
                } else {
                    y_harm
                }
            } else {
                // Mantiene actualizados los filtros aunque estemos en Optical para transiciones suaves.
                let _ = self.xover_lp.process_sample_f64(x);
                let _ = self.xover_hp.process_sample_f64(x);
                x * g_low
            };

            *s = (wet * lvl) as f32;
        }
    }

    fn reset(&mut self) {
        self.xover_lp.reset();
        self.xover_hp.reset();
        self.lfo_phase = 0.0;
        self.mode_xfade
            .reset(TREMOLO_PARAMS[0].step_index(self.mode));
        self.rate_s.reset(self.rate_hz);
        self.depth_s.reset(self.depth * 0.01);
        self.shape_s.reset(self.shape * 0.01);
        self.level_s.reset(db_to_lin(self.level_db));
    }
}
