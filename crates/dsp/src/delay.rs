//! Pedal **Delay** multimodo (`Digital` / `Analog BBD` / `Tape` — `D17` Opción A).
//!
//! Comparte una única [`FractionalDelayLine`] preasignada en `prepare()` (cubre `20–1500 ms` más
//! modulación hasta `192 kHz`) y suavizado de `Time` para deslizamiento continuo de tono:
//! - **Digital:** lazo de realimentación lineal de banda completa con techo de seguridad suave.
//! - **Analog BBD *(Raffel & Smith 2010)*:** pasa-altos + filtro multi-polo de reconstrucción BBD +
//!   compresión suave en cada repetición para oscurecer los ecos gradualmente.
//! - **Tape Delay *(Chowdhury 2019)*:** pasa-altos + pasa-bajos de cabezal + saturación asimétrica
//!   cálida de cinta + modulación combinada *wow & flutter* en el cabezal de lectura.

use std::f64::consts::TAU;

use crate::delay_line::FractionalDelayLine;
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::svf::{SvfMode, SvfTpt};
use crate::{sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const DELAY_MODES: &[&str] = &["Digital", "Analog BBD", "Tape"];
const MAX_TIME_SECONDS: f32 = 1.60;
const DELAY_SUBBLOCK: usize = 16;

const DELAY_PARAMS: [ParamSpec; 6] = [
    ParamSpec {
        id: "mode",
        name: "Mode",
        unit: "",
        min: 0.0,
        max: 2.0,
        default: 0.0,
        scale: ParamScale::Stepped {
            labels: DELAY_MODES,
        },
        decimals: 0,
    },
    ParamSpec {
        id: "time_ms",
        name: "Time",
        unit: "ms",
        min: 20.0,
        max: 1_500.0,
        default: 380.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "feedback",
        name: "Feedback",
        unit: "%",
        min: 0.0,
        max: 95.0,
        default: 38.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "tone",
        name: "Tone",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 55.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "mod_depth",
        name: "Mod",
        unit: "%",
        min: 0.0,
        max: 100.0,
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
        default: 32.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
];

#[derive(Debug, Clone)]
struct LoopVoice {
    dc: DcBlocker,
    hp: SvfTpt,
    lp: SvfTpt,
}

impl LoopVoice {
    fn new(sr: f32) -> Self {
        let mut v = Self {
            dc: DcBlocker::new(20.0),
            hp: SvfTpt::new(sr),
            lp: SvfTpt::new(sr),
        };
        v.prepare(sr);
        v
    }

    fn prepare(&mut self, sr: f32) {
        self.dc.prepare(sr);
        self.hp.set_sample_rate(sr);
        self.lp.set_sample_rate(sr);
        self.reset();
    }

    fn reset(&mut self) {
        self.dc.reset();
        self.hp.reset();
        self.lp.reset();
    }

    #[inline]
    fn update_filters(&mut self, mode: usize, tone_norm: f32, sr: f32) {
        let t = tone_norm.clamp(0.0, 1.0);
        match mode {
            0 => {
                // Digital: respuesta amplia (solo recorte suave muy arriba si Tone baja).
                let fc = (4_500.0 + t * 13_500.0).min(0.46 * sr);
                self.lp.set_params(SvfMode::LowPass, fc, 0.707, 0.0);
            }
            1 => {
                // Analog BBD: limpia subsónicos y oscurece progresivamente los ecos.
                self.hp
                    .set_params(SvfMode::HighPass, 75.0_f32.min(0.4 * sr), 0.707, 0.0);
                let fc = (1_400.0 + t * 2_600.0).min(0.44 * sr);
                self.lp.set_params(SvfMode::LowPass, fc, 0.75, 0.0);
            }
            _ => {
                // Tape: roll-off cálido de cabezal de cinta y realce de cuerpo.
                self.hp
                    .set_params(SvfMode::HighPass, 95.0_f32.min(0.4 * sr), 0.707, 0.0);
                let fc = (1_800.0 + t * 3_000.0).min(0.44 * sr);
                self.lp.set_params(SvfMode::LowPass, fc, 0.707, 0.0);
            }
        }
    }

    #[inline]
    fn process(&mut self, mode: usize, delayed: f32) -> f32 {
        let x = self.dc.process_sample(delayed) as f64;
        match mode {
            0 => {
                let y = self.lp.process_sample_f64(x);
                (1.5 * (y / 1.5).tanh()) as f32
            }
            1 => {
                let h = self.hp.process_sample_f64(x);
                let y = self.lp.process_sample_f64(h);
                ((1.25 * y).tanh() / 1.25) as f32
            }
            _ => {
                let h = self.hp.process_sample_f64(x);
                let y = self.lp.process_sample_f64(h);
                let asym = ((1.35 * y + 0.06).tanh() - 0.06_f64.tanh()) / 1.35;
                asym as f32
            }
        }
    }
}

/// Delay multimodo (`Digital`, `Analog BBD`, `Tape`) sobre línea de retardo Hermite compartida.
pub struct Delay {
    mode: f32,
    time_ms: f32,
    feedback: f32,
    tone: f32,
    mod_depth: f32,
    mix: f32,
    sample_rate: f32,

    line: FractionalDelayLine,
    voices: [LoopVoice; 3],
    mode_xfade: ModeCrossfader,

    time_s: OnePoleSmoother,
    fb_s: OnePoleSmoother,
    tone_s: OnePoleSmoother,
    mod_s: OnePoleSmoother,
    mix_s: OnePoleSmoother,

    wow_phase: f64,
    flutter_phase: f64,
    sub_counter: usize,
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

impl Delay {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let def_mode = DELAY_PARAMS[0].step_index(DELAY_PARAMS[0].default);
        let mut d = Self {
            mode: DELAY_PARAMS[0].default,
            time_ms: DELAY_PARAMS[1].default,
            feedback: DELAY_PARAMS[2].default,
            tone: DELAY_PARAMS[3].default,
            mod_depth: DELAY_PARAMS[4].default,
            mix: DELAY_PARAMS[5].default,
            sample_rate: sr,
            line: FractionalDelayLine::new(),
            voices: [LoopVoice::new(sr), LoopVoice::new(sr), LoopVoice::new(sr)],
            mode_xfade: ModeCrossfader::new(8.0, sr, def_mode),
            time_s: OnePoleSmoother::new(45.0, sr, DELAY_PARAMS[1].default),
            fb_s: OnePoleSmoother::new(15.0, sr, DELAY_PARAMS[2].default * 0.01),
            tone_s: OnePoleSmoother::new(15.0, sr, DELAY_PARAMS[3].default * 0.01),
            mod_s: OnePoleSmoother::new(15.0, sr, DELAY_PARAMS[4].default * 0.01),
            mix_s: OnePoleSmoother::new(10.0, sr, DELAY_PARAMS[5].default * 0.01),
            wow_phase: 0.0,
            flutter_phase: 0.0,
            sub_counter: 0,
        };
        d.prepare(sr, 512);
        d
    }

    pub fn set_mode(&mut self, mode: f32) {
        self.set_param(0, mode);
    }

    pub fn set_time_ms(&mut self, ms: f32) {
        self.set_param(1, ms);
    }

    pub fn set_feedback(&mut self, pct: f32) {
        self.set_param(2, pct);
    }

    pub fn set_tone(&mut self, pct: f32) {
        self.set_param(3, pct);
    }

    pub fn set_mod_depth(&mut self, pct: f32) {
        self.set_param(4, pct);
    }

    pub fn set_mix(&mut self, pct: f32) {
        self.set_param(5, pct);
    }

    #[inline]
    fn mod_excursion_ms(mode_idx: usize) -> f64 {
        match mode_idx {
            0 => 0.35,
            1 => 1.05,
            _ => 1.85,
        }
    }
}

impl Effect for Delay {
    fn kind(&self) -> &'static str {
        "delay"
    }

    fn name(&self) -> &'static str {
        "Delay"
    }

    fn accent_color(&self) -> [u8; 3] {
        [59, 130, 246]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &DELAY_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.mode,
            1 => self.time_ms,
            2 => self.feedback,
            3 => self.tone,
            4 => self.mod_depth,
            5 => self.mix,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = DELAY_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                self.mode = v;
                self.mode_xfade.set_mode(spec.step_index(v));
            }
            1 => {
                self.time_ms = v;
                self.time_s.set_target(v);
            }
            2 => {
                self.feedback = v;
                self.fb_s.set_target(v * 0.01);
            }
            3 => {
                self.tone = v;
                self.tone_s.set_target(v * 0.01);
            }
            4 => {
                self.mod_depth = v;
                self.mod_s.set_target(v * 0.01);
            }
            5 => {
                self.mix = v;
                self.mix_s.set_target(v * 0.01);
            }
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        let max_samples = (MAX_TIME_SECONDS * sr).ceil() as usize + 32;
        self.line.prepare(max_samples);
        for v in &mut self.voices {
            v.prepare(sr);
        }
        self.mode_xfade.set_sample_rate(sr);
        self.time_s.set_sample_rate(sr);
        self.fb_s.set_sample_rate(sr);
        self.tone_s.set_sample_rate(sr);
        self.mod_s.set_sample_rate(sr);
        self.mix_s.set_sample_rate(sr);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sr = (self.sample_rate as f64).max(1.0);
        let sr_f32 = sr as f32;
        let max_safe_delay = (self.line.capacity().saturating_sub(6)) as f64;

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s);
            let time_ms = self.time_s.next_f64();
            let fb_gain = self.fb_s.next_val().clamp(0.0, 0.95);
            let tone_norm = self.tone_s.next_val();
            let mod_norm = self.mod_s.next_f64();
            let mix = self.mix_s.next_val();

            let wow = (TAU * self.wow_phase).sin();
            let flutter = (TAU * self.flutter_phase).sin();
            self.wow_phase = (self.wow_phase + 0.55 / sr).fract();
            self.flutter_phase = (self.flutter_phase + 5.80 / sr).fract();

            let xfade = self.mode_xfade.is_crossfading();
            let (prev_m, next_m) = self.mode_xfade.pair();

            if self.sub_counter == 0 {
                if xfade && prev_m != next_m {
                    self.voices[prev_m].update_filters(prev_m, tone_norm, sr_f32);
                    self.voices[next_m].update_filters(next_m, tone_norm, sr_f32);
                } else {
                    self.voices[next_m].update_filters(next_m, tone_norm, sr_f32);
                }
            }
            self.sub_counter = (self.sub_counter + 1) % DELAY_SUBBLOCK;

            let exc_ms = Self::mod_excursion_ms(next_m);
            let mod_ms = (0.65 * wow + 0.35 * flutter) * exc_ms * mod_norm;
            let delay_samples = ((time_ms + mod_ms) * 1e-3 * sr).clamp(4.0, max_safe_delay);
            let delayed = self.line.read_before_push_hermite(delay_samples);

            let loop_sig = if xfade && prev_m != next_m {
                let y_p = self.voices[prev_m].process(prev_m, delayed);
                let y_n = self.voices[next_m].process(next_m, delayed);
                self.mode_xfade.mix_sample(y_p, y_n)
            } else {
                self.voices[next_m].process(next_m, delayed)
            };

            self.line.push(x + fb_gain * loop_sig);
            *s = x * (1.0 - mix) + loop_sig * mix;
        }
    }

    fn reset(&mut self) {
        self.line.reset();
        for (idx, v) in self.voices.iter_mut().enumerate() {
            v.reset();
            v.update_filters(idx, self.tone * 0.01, self.sample_rate);
        }
        self.mode_xfade.reset(DELAY_PARAMS[0].step_index(self.mode));
        self.time_s.reset(self.time_ms);
        self.fb_s.reset(self.feedback * 0.01);
        self.tone_s.reset(self.tone * 0.01);
        self.mod_s.reset(self.mod_depth * 0.01);
        self.mix_s.reset(self.mix * 0.01);
        self.wow_phase = 0.0;
        self.flutter_phase = 0.0;
        self.sub_counter = 0;
    }
}
