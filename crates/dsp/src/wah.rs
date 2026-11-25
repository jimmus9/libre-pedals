//! Pedal **Wah / Auto-Wah (*Envelope Filter*)** (`D14` Opción A).
//!
//! Basado en el modelado físico nodal del **Dunlop CryBaby GCB-95** (Holters & Zölzer, DAFx):
//! - La frecuencia central vocal barre exponencialmente entre `~350 Hz` (talón) y `~2.2 kHz` (punta).
//! - El factor de calidad $Q$ y la ganancia pico están acoplados al barrido (se estrechan y elevan
//!   al subir hacia la zona vocal media-alta, recreando la red RC con inductor Fasel).
//! - Implementación sobre [`SvfTpt`] trapezoidal (estable bajo modulación por muestra).
//! - Conmutación `Stepped` sin clics mediante [`ModeCrossfader`] (`5 ms`) entre `Manual`, `Auto Up`
//!   y `Auto Down`.

use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::svf::{SvfMode, SvfTpt};
use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const WAH_MODES: &[&str] = &["Manual", "Auto Up", "Auto Down"];
const WAH_SUBBLOCK: usize = 8;

const WAH_PARAMS: [ParamSpec; 6] = [
    ParamSpec {
        id: "mode",
        name: "Mode",
        unit: "",
        min: 0.0,
        max: 2.0,
        default: 1.0,
        scale: ParamScale::Stepped { labels: WAH_MODES },
        decimals: 0,
    },
    ParamSpec {
        id: "position",
        name: "Pedal",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 35.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "sensitivity",
        name: "Sens",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 65.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "resonance",
        name: "Resonance",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "release_ms",
        name: "Release",
        unit: "ms",
        min: 30.0,
        max: 600.0,
        default: 160.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "mix",
        name: "Mix",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
];

#[derive(Debug, Clone)]
struct WahBranch {
    bp_svf: SvfTpt,
    body_lp: SvfTpt,
    peak_gain: f64,
}

impl WahBranch {
    fn new(sample_rate: f32) -> Self {
        let mut b = Self {
            bp_svf: SvfTpt::new(sample_rate),
            body_lp: SvfTpt::new(sample_rate),
            peak_gain: 1.5,
        };
        b.prepare(sample_rate);
        b
    }

    fn prepare(&mut self, sample_rate: f32) {
        self.bp_svf.set_sample_rate(sample_rate);
        self.body_lp.set_sample_rate(sample_rate);
        self.body_lp.set_params(
            SvfMode::LowPass,
            260.0_f32.min(0.45 * sample_rate),
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        self.reset();
    }

    fn reset(&mut self) {
        self.bp_svf.reset();
        self.body_lp.reset();
    }

    /// Acopla frecuencia central, factor Q y ganancia pico del filtro CryBaby según `pos` y `res_norm`.
    #[inline]
    fn update_crybaby(&mut self, pos: f32, res_norm: f32, sample_rate: f32) {
        let p = pos.clamp(0.0, 1.0);
        let r = res_norm.clamp(0.0, 1.0);
        let fc = (350.0 * (2_200.0_f32 / 350.0).powf(p)).min(0.45 * sample_rate);
        let q = (2.0 + 3.8 * p.powf(0.85)) * (0.55 + 0.90 * r);
        self.peak_gain = ((1.15 + 0.75 * p) * (0.85 + 0.35 * r)) as f64;
        self.bp_svf.set_params(SvfMode::BandPass, fc, q, 0.0);
    }

    #[inline]
    fn process_sample(&mut self, x: f64) -> f64 {
        let bp = self.bp_svf.process_sample_f64(x);
        let body = self.body_lp.process_sample_f64(x);
        bp * self.peak_gain + 0.16 * body
    }
}

/// Pedal Wah / Auto-Wah CryBaby GCB-95 sobre `SvfTpt` con 3 modos conmutados sin clics.
pub struct Wah {
    mode: f32,
    position: f32,
    sensitivity: f32,
    resonance: f32,
    release_ms: f32,
    mix: f32,
    sample_rate: f32,

    mode_xfade: ModeCrossfader,
    pos_smoother: OnePoleSmoother,
    res_smoother: OnePoleSmoother,
    mix_smoother: OnePoleSmoother,

    env: f64,
    env_attack_coeff: f64,
    env_release_coeff: f64,

    branches: [WahBranch; 3],
    sub_counter: usize,
}

impl Default for Wah {
    fn default() -> Self {
        Self::new()
    }
}

impl Wah {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let default_mode = WAH_PARAMS[0].step_index(WAH_PARAMS[0].default);
        let mut w = Self {
            mode: WAH_PARAMS[0].default,
            position: WAH_PARAMS[1].default,
            sensitivity: WAH_PARAMS[2].default,
            resonance: WAH_PARAMS[3].default,
            release_ms: WAH_PARAMS[4].default,
            mix: WAH_PARAMS[5].default,
            sample_rate: sr,
            mode_xfade: ModeCrossfader::new(5.0, sr, default_mode),
            pos_smoother: OnePoleSmoother::new(8.0, sr, WAH_PARAMS[1].default * 0.01),
            res_smoother: OnePoleSmoother::new(10.0, sr, WAH_PARAMS[3].default * 0.01),
            mix_smoother: OnePoleSmoother::new(10.0, sr, WAH_PARAMS[5].default * 0.01),
            env: 0.0,
            env_attack_coeff: 0.0,
            env_release_coeff: 0.0,
            branches: [WahBranch::new(sr), WahBranch::new(sr), WahBranch::new(sr)],
            sub_counter: 0,
        };
        w.prepare(sr, 512);
        w
    }

    pub fn set_mode(&mut self, mode: f32) {
        self.set_param(0, mode);
    }

    pub fn set_position(&mut self, pct: f32) {
        self.set_param(1, pct);
    }

    pub fn set_sensitivity(&mut self, pct: f32) {
        self.set_param(2, pct);
    }

    pub fn set_resonance(&mut self, pct: f32) {
        self.set_param(3, pct);
    }

    pub fn set_release_ms(&mut self, ms: f32) {
        self.set_param(4, ms);
    }

    pub fn set_mix(&mut self, pct: f32) {
        self.set_param(5, pct);
    }

    #[inline]
    fn recompute_env_coeffs(&mut self) {
        let sr = (self.sample_rate as f64).max(1.0);
        self.env_attack_coeff = (-1.0 / (0.0035 * sr)).exp();
        let rel_s = (self.release_ms as f64 * 1e-3).max(0.005);
        self.env_release_coeff = (-1.0 / (rel_s * sr)).exp();
    }

    /// Calcula la posición efectiva del barrido `[0, 1]` para un modo dado.
    ///
    /// - `Manual (0)`: sigue directamente `Pedal` (`pos_base`).
    /// - `Auto Up (1)`: barre ascendente desde `pos_base` hacia `1.0` según la envolvente.
    /// - `Auto Down (2)`: parte desde una altura continua `p_top = 0.35 + 0.65 * pos_base`
    ///   (ej. `~0.58` con el default `35%`, `1.0` con `100%`) y cae hacia el talón (`0.0`).
    #[inline]
    fn sweep_position_for_mode(mode_idx: usize, pos_base: f32, env_mapped: f32) -> f32 {
        match mode_idx {
            0 => pos_base.clamp(0.0, 1.0),
            1 => (pos_base + (1.0 - pos_base) * env_mapped).clamp(0.0, 1.0),
            _ => {
                let p_top = (0.35 + 0.65 * pos_base).clamp(0.0, 1.0);
                (p_top * (1.0 - env_mapped)).clamp(0.0, 1.0)
            }
        }
    }
}

impl Effect for Wah {
    fn kind(&self) -> &'static str {
        "wah"
    }

    fn name(&self) -> &'static str {
        "Wah / Auto-Wah"
    }

    fn accent_color(&self) -> [u8; 3] {
        [168, 85, 247]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &WAH_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.mode,
            1 => self.position,
            2 => self.sensitivity,
            3 => self.resonance,
            4 => self.release_ms,
            5 => self.mix,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = WAH_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                self.mode = v;
                let idx = spec.step_index(v);
                self.mode_xfade.set_mode(idx);
            }
            1 => {
                self.position = v;
                self.pos_smoother.set_target(v * 0.01);
            }
            2 => self.sensitivity = v,
            3 => {
                self.resonance = v;
                self.res_smoother.set_target(v * 0.01);
            }
            4 => {
                if (self.release_ms - v).abs() > 1e-4 {
                    self.release_ms = v;
                    self.recompute_env_coeffs();
                }
            }
            5 => {
                self.mix = v;
                self.mix_smoother.set_target(v * 0.01);
            }
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        self.mode_xfade.set_sample_rate(sr);
        self.pos_smoother.set_sample_rate(sr);
        self.res_smoother.set_sample_rate(sr);
        self.mix_smoother.set_sample_rate(sr);
        self.recompute_env_coeffs();
        for branch in &mut self.branches {
            branch.prepare(sr);
        }
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sens_norm = self.sensitivity * 0.01;
        let sens_gain = db_to_lin(-6.0 + 34.0 * sens_norm) as f64;
        let sr = self.sample_rate;

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s) as f64;
            let abs_x = x.abs();

            if abs_x > self.env {
                self.env = self.env_attack_coeff * self.env + (1.0 - self.env_attack_coeff) * abs_x;
            } else {
                self.env =
                    self.env_release_coeff * self.env + (1.0 - self.env_release_coeff) * abs_x;
            }
            if self.env < 1e-12 {
                self.env = 0.0;
            }
            let env_mapped = (1.0 - (-self.env * sens_gain).exp()) as f32;

            let pos_base = self.pos_smoother.next_val();
            let res_norm = self.res_smoother.next_val();
            let mix = self.mix_smoother.next_f64();

            let xfade_active = self.mode_xfade.is_crossfading();
            let (prev_mode, next_mode) = self.mode_xfade.pair();

            if self.sub_counter == 0 {
                if xfade_active && prev_mode != next_mode {
                    let p_prev = Self::sweep_position_for_mode(prev_mode, pos_base, env_mapped);
                    let p_next = Self::sweep_position_for_mode(next_mode, pos_base, env_mapped);
                    self.branches[prev_mode].update_crybaby(p_prev, res_norm, sr);
                    self.branches[next_mode].update_crybaby(p_next, res_norm, sr);
                } else {
                    let p_act = Self::sweep_position_for_mode(next_mode, pos_base, env_mapped);
                    self.branches[next_mode].update_crybaby(p_act, res_norm, sr);
                }
            }
            self.sub_counter = (self.sub_counter + 1) % WAH_SUBBLOCK;

            let wet = if xfade_active && prev_mode != next_mode {
                let y_prev = self.branches[prev_mode].process_sample(x) as f32;
                let y_next = self.branches[next_mode].process_sample(x) as f32;
                self.mode_xfade.mix_sample(y_prev, y_next) as f64
            } else {
                self.branches[next_mode].process_sample(x)
            };

            *s = (x * (1.0 - mix) + wet * mix) as f32;
        }
    }

    fn reset(&mut self) {
        let mode_idx = WAH_PARAMS[0].step_index(self.mode);
        self.mode_xfade.reset(mode_idx);
        self.pos_smoother.reset(self.position * 0.01);
        self.res_smoother.reset(self.resonance * 0.01);
        self.mix_smoother.reset(self.mix * 0.01);
        self.env = 0.0;
        self.sub_counter = 0;

        let res_norm = self.resonance * 0.01;
        let pos_base = self.position * 0.01;
        for (idx, branch) in self.branches.iter_mut().enumerate() {
            branch.reset();
            let p = Self::sweep_position_for_mode(idx, pos_base, 0.0);
            branch.update_crybaby(p, res_norm, self.sample_rate);
        }
    }
}
