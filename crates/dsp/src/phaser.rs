//! Pedal **Phaser** *Gray-Box* de 4 o 6 etapas *all-pass* TPT (*estilo MXR Phase 90*, Kiiski et al. DAFx-16).
//!
//! - Cascada de hasta 6 filtros [`OnePoleAllPassTpt`] con barrido exponencial por LFO entre
//!   `~215 Hz` y `~3.44 kHz`, creando 2 o 3 muescas profundas en movimiento al sumar con la señal seca.
//! - Conmutación sin clics entre `4-Stage` y `6-Stage` mediante [`ModeCrossfader`] (`5 ms`).
//! - Lazo de realimentación acotado con saturación suave y [`DcBlocker`] + `flush_denormal`.

use std::f64::consts::TAU;

use crate::delay_line::OnePoleAllPassTpt;
use crate::denormals::flush_denormal;
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::{sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const PHASER_STAGES: &[&str] = &["4-Stage", "6-Stage"];
const PHASER_SUBBLOCK: usize = 4;

const PHASER_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "stages",
        name: "Stages",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        scale: ParamScale::Stepped {
            labels: PHASER_STAGES,
        },
        decimals: 0,
    },
    ParamSpec {
        id: "rate_hz",
        name: "Rate",
        unit: "Hz",
        min: 0.08,
        max: 8.0,
        default: 0.60,
        scale: ParamScale::Logarithmic,
        decimals: 2,
    },
    ParamSpec {
        id: "depth",
        name: "Depth",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 75.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "feedback",
        name: "Feedback",
        unit: "%",
        min: 0.0,
        max: 85.0,
        default: 25.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "mix",
        name: "Mix",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
];

/// Phaser TPT de 4 o 6 etapas *all-pass* con realimentación y transición `Stepped` sin clics.
pub struct Phaser {
    stages: f32,
    rate_hz: f32,
    depth: f32,
    feedback: f32,
    mix: f32,
    sample_rate: f32,

    ap: [OnePoleAllPassTpt; 6],
    fb_dc: DcBlocker,
    fb_state: f64,

    stage_xfade: ModeCrossfader,
    rate_s: OnePoleSmoother,
    depth_s: OnePoleSmoother,
    fb_s: OnePoleSmoother,
    mix_s: OnePoleSmoother,

    lfo_phase: f64,
    sub_counter: usize,
}

impl Default for Phaser {
    fn default() -> Self {
        Self::new()
    }
}

impl Phaser {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let def_mode = PHASER_PARAMS[0].step_index(PHASER_PARAMS[0].default);
        let mut p = Self {
            stages: PHASER_PARAMS[0].default,
            rate_hz: PHASER_PARAMS[1].default,
            depth: PHASER_PARAMS[2].default,
            feedback: PHASER_PARAMS[3].default,
            mix: PHASER_PARAMS[4].default,
            sample_rate: sr,
            ap: [OnePoleAllPassTpt::new(); 6],
            fb_dc: DcBlocker::new(20.0),
            fb_state: 0.0,
            stage_xfade: ModeCrossfader::new(5.0, sr, def_mode),
            rate_s: OnePoleSmoother::new(15.0, sr, PHASER_PARAMS[1].default),
            depth_s: OnePoleSmoother::new(15.0, sr, PHASER_PARAMS[2].default * 0.01),
            fb_s: OnePoleSmoother::new(15.0, sr, PHASER_PARAMS[3].default * 0.01),
            mix_s: OnePoleSmoother::new(10.0, sr, PHASER_PARAMS[4].default * 0.01),
            lfo_phase: 0.0,
            sub_counter: 0,
        };
        p.prepare(sr, 512);
        p
    }

    pub fn set_stages(&mut self, stages: f32) {
        self.set_param(0, stages);
    }

    pub fn set_rate_hz(&mut self, hz: f32) {
        self.set_param(1, hz);
    }

    pub fn set_depth(&mut self, pct: f32) {
        self.set_param(2, pct);
    }

    pub fn set_feedback(&mut self, pct: f32) {
        self.set_param(3, pct);
    }

    pub fn set_mix(&mut self, pct: f32) {
        self.set_param(4, pct);
    }

    /// Fija la frecuencia de quiebre manual instantánea de todas las etapas (útil para inspeccionar muescas).
    #[inline]
    pub fn current_lfo_freq_hz(&self) -> f64 {
        let lfo_sin = (TAU * self.lfo_phase).sin();
        860.0 * 4.0_f64.powf((self.depth * 0.01) as f64 * lfo_sin)
    }
}

impl Effect for Phaser {
    fn kind(&self) -> &'static str {
        "phaser"
    }

    fn name(&self) -> &'static str {
        "Phaser"
    }

    fn accent_color(&self) -> [u8; 3] {
        [245, 158, 11]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &PHASER_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.stages,
            1 => self.rate_hz,
            2 => self.depth,
            3 => self.feedback,
            4 => self.mix,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = PHASER_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                self.stages = v;
                self.stage_xfade.set_mode(spec.step_index(v));
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
                self.feedback = v;
                self.fb_s.set_target(v * 0.01);
            }
            4 => {
                self.mix = v;
                self.mix_s.set_target(v * 0.01);
            }
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        self.fb_dc.prepare(sr);
        self.stage_xfade.set_sample_rate(sr);
        self.rate_s.set_sample_rate(sr);
        self.depth_s.set_sample_rate(sr);
        self.fb_s.set_sample_rate(sr);
        self.mix_s.set_sample_rate(sr);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sr = (self.sample_rate as f64).max(1.0);

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s) as f64;
            let rate = self.rate_s.next_f64();
            let depth = self.depth_s.next_f64();
            let fb_gain = (self.fb_s.next_f64() * (0.82 / 0.85)).clamp(0.0, 0.82);
            let mix = self.mix_s.next_f64();

            let lfo_sin = (TAU * self.lfo_phase).sin();
            self.lfo_phase = (self.lfo_phase + rate / sr).fract();

            if self.sub_counter == 0 {
                let fc = 860.0 * 4.0_f64.powf(depth * lfo_sin);
                for stage in &mut self.ap {
                    stage.set_cutoff(fc, sr);
                }
            }
            self.sub_counter = (self.sub_counter + 1) % PHASER_SUBBLOCK;

            let u = x - (fb_gain * self.fb_state).tanh();
            let mut sig = u;
            for stage in self.ap.iter_mut().take(4) {
                sig = stage.process_sample(sig);
            }
            let y4 = sig;
            sig = self.ap[4].process_sample(sig);
            let y6 = self.ap[5].process_sample(sig);

            let (prev_m, next_m) = self.stage_xfade.pair();
            let y_prev = if prev_m == 0 { y4 } else { y6 } as f32;
            let y_next = if next_m == 0 { y4 } else { y6 } as f32;
            let y_sel = self.stage_xfade.mix_sample(y_prev, y_next);

            self.fb_state = flush_denormal(self.fb_dc.process_sample(y_sel)) as f64;
            *s = (x * (1.0 - mix) + (y_sel as f64) * mix) as f32;
        }
    }

    fn reset(&mut self) {
        for stage in &mut self.ap {
            stage.reset();
            stage.set_cutoff(860.0, self.sample_rate as f64);
        }
        self.fb_dc.reset();
        self.fb_state = 0.0;
        self.lfo_phase = 0.0;
        self.sub_counter = 0;

        self.stage_xfade
            .reset(PHASER_PARAMS[0].step_index(self.stages));
        self.rate_s.reset(self.rate_hz);
        self.depth_s.reset(self.depth * 0.01);
        self.fb_s.reset(self.feedback * 0.01);
        self.mix_s.reset(self.mix * 0.01);
    }
}
