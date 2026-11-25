//! Pedal **Chorus** analógico BBD (*estilo Boss CE-2*, Raffel & Smith DAFx-10).
//!
//! - Línea de retardo fraccional modulada mediante interpolación cúbica de Hermite de 4 puntos
//!   ([`FractionalDelayLine`]), eliminando saltos o artefactos de cambio de velocidad.
//! - Coloración BBD con pasa-bajos analógico [`SvfTpt`] (`Warmth`) y saturación sutil en la rama húmeda.

use std::f64::consts::TAU;

use crate::delay_line::FractionalDelayLine;
use crate::smoother::OnePoleSmoother;
use crate::svf::{SvfMode, SvfTpt};
use crate::{sanitize_sample, Effect, ParamScale, ParamSpec};

const CHORUS_SUBBLOCK: usize = 16;

const CHORUS_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "rate_hz",
        name: "Rate",
        unit: "Hz",
        min: 0.1,
        max: 6.0,
        default: 0.85,
        scale: ParamScale::Logarithmic,
        decimals: 2,
    },
    ParamSpec {
        id: "depth",
        name: "Depth",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 55.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "center_ms",
        name: "Center",
        unit: "ms",
        min: 4.0,
        max: 15.0,
        default: 7.5,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "warmth_hz",
        name: "Warmth",
        unit: "Hz",
        min: 3_000.0,
        max: 14_000.0,
        default: 8_000.0,
        scale: ParamScale::Logarithmic,
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

/// Chorus BBD de retardo fraccional Hermite con filtrado cálido en rama húmeda.
pub struct Chorus {
    rate_hz: f32,
    depth: f32,
    center_ms: f32,
    warmth_hz: f32,
    mix: f32,
    sample_rate: f32,

    delay_line: FractionalDelayLine,
    warmth_lp: SvfTpt,

    rate_s: OnePoleSmoother,
    depth_s: OnePoleSmoother,
    center_s: OnePoleSmoother,
    warmth_s: OnePoleSmoother,
    mix_s: OnePoleSmoother,

    lfo_phase: f64,
    sub_counter: usize,
}

impl Default for Chorus {
    fn default() -> Self {
        Self::new()
    }
}

impl Chorus {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let mut c = Self {
            rate_hz: CHORUS_PARAMS[0].default,
            depth: CHORUS_PARAMS[1].default,
            center_ms: CHORUS_PARAMS[2].default,
            warmth_hz: CHORUS_PARAMS[3].default,
            mix: CHORUS_PARAMS[4].default,
            sample_rate: sr,
            delay_line: FractionalDelayLine::new(),
            warmth_lp: SvfTpt::new(sr),
            rate_s: OnePoleSmoother::new(15.0, sr, CHORUS_PARAMS[0].default),
            depth_s: OnePoleSmoother::new(15.0, sr, CHORUS_PARAMS[1].default * 0.01),
            center_s: OnePoleSmoother::new(20.0, sr, CHORUS_PARAMS[2].default),
            warmth_s: OnePoleSmoother::new(15.0, sr, CHORUS_PARAMS[3].default),
            mix_s: OnePoleSmoother::new(10.0, sr, CHORUS_PARAMS[4].default * 0.01),
            lfo_phase: 0.0,
            sub_counter: 0,
        };
        c.prepare(sr, 512);
        c
    }

    pub fn set_rate_hz(&mut self, hz: f32) {
        self.set_param(0, hz);
    }

    pub fn set_depth(&mut self, pct: f32) {
        self.set_param(1, pct);
    }

    pub fn set_center_ms(&mut self, ms: f32) {
        self.set_param(2, ms);
    }

    pub fn set_warmth_hz(&mut self, hz: f32) {
        self.set_param(3, hz);
    }

    pub fn set_mix(&mut self, pct: f32) {
        self.set_param(4, pct);
    }

    #[inline]
    fn ce2_lfo(phase: f64) -> f64 {
        let tri = 1.0 - 4.0 * (phase - 0.5).abs();
        0.7 * (TAU * phase).sin() + 0.3 * tri
    }
}

impl Effect for Chorus {
    fn kind(&self) -> &'static str {
        "chorus"
    }

    fn name(&self) -> &'static str {
        "Chorus"
    }

    fn accent_color(&self) -> [u8; 3] {
        [14, 165, 233]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &CHORUS_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.rate_hz,
            1 => self.depth,
            2 => self.center_ms,
            3 => self.warmth_hz,
            4 => self.mix,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = CHORUS_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                self.rate_hz = v;
                self.rate_s.set_target(v);
            }
            1 => {
                self.depth = v;
                self.depth_s.set_target(v * 0.01);
            }
            2 => {
                self.center_ms = v;
                self.center_s.set_target(v);
            }
            3 => {
                self.warmth_hz = v;
                self.warmth_s.set_target(v);
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
        let max_samples = (0.030 * sr).ceil() as usize + 8;
        self.delay_line.prepare(max_samples);
        self.warmth_lp.set_sample_rate(sr);
        self.rate_s.set_sample_rate(sr);
        self.depth_s.set_sample_rate(sr);
        self.center_s.set_sample_rate(sr);
        self.warmth_s.set_sample_rate(sr);
        self.mix_s.set_sample_rate(sr);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sr = (self.sample_rate as f64).max(1.0);
        let sr_f32 = sr as f32;
        let q = std::f32::consts::FRAC_1_SQRT_2;

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s);
            let rate = self.rate_s.next_f64();
            let depth = self.depth_s.next_f64();
            let center_ms = self.center_s.next_f64();
            let warmth = self.warmth_s.next_val();
            let mix = self.mix_s.next_val();

            if self.sub_counter == 0 {
                self.warmth_lp.set_params(
                    SvfMode::LowPass,
                    warmth.clamp(500.0, 0.45 * sr_f32),
                    q,
                    0.0,
                );
            }
            self.sub_counter = (self.sub_counter + 1) % CHORUS_SUBBLOCK;

            let lfo = Self::ce2_lfo(self.lfo_phase);
            self.lfo_phase = (self.lfo_phase + rate / sr).fract();

            let delay_ms = (center_ms * (1.0 + 0.35 * depth * lfo)).clamp(1.0, 25.0);
            let delay_samples = delay_ms * 1e-3 * sr;

            self.delay_line.push(x);
            let delayed = self.delay_line.read_after_push_hermite(delay_samples);
            let filtered = self.warmth_lp.process_sample_f64(delayed as f64);
            let wet = ((filtered * 1.1).tanh() / 1.1) as f32;

            *s = x * (1.0 - mix) + wet * mix;
        }
    }

    fn reset(&mut self) {
        self.delay_line.reset();
        self.rate_s.reset(self.rate_hz);
        self.depth_s.reset(self.depth * 0.01);
        self.center_s.reset(self.center_ms);
        self.warmth_s.reset(self.warmth_hz);
        self.mix_s.reset(self.mix * 0.01);
        self.lfo_phase = 0.0;
        self.sub_counter = 0;

        self.warmth_lp.set_params(
            SvfMode::LowPass,
            self.warmth_hz.clamp(500.0, 0.45 * self.sample_rate),
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        self.warmth_lp.reset();
    }
}
