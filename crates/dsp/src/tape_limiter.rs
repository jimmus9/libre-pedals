//! Soft Clipper $C^2$ paramétrico / Limitador de Cinta (*Tape Limiter*) (`D12`):
//! - 4 parámetros: `Drive`, `Ceiling`, `Knee` y selector `Stepped` (`Soft Clip` / `Tape` con
//!   crossfade `6 ms`).
//! - Modo `Soft Clip`: transferencia lineal hasta `(C - K)` + rodilla hiperbólica suave hasta la
//!   asíntota exacta `Ceiling` ($C$), con `ADAA1` analítica en `f64` dentro de `Oversampler` $M(f_s)$.
//! - Modo `Tape`: pre-énfasis `MatchedBiquad` (`3 kHz`), saturación asimétrica `AsymSoftClipAdaa1`
//!   en `Oversampler`, `DcBlocker(10 Hz)` y de-énfasis complementario para lograr compresión de
//!   agudos y calidez `H2/H3` natural.

use crate::adaa::{ln_cosh_stable, AsymSoftClipAdaa1};
use crate::matched_eq::{MatchedBiquad, MatchedFilterKind};
use crate::oversampler::Oversampler;
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::{db_to_lin, sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const MODE_LABELS: [&str; 2] = ["Soft Clip", "Tape"];

const TAPE_LIMITER_PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        id: "drive_db",
        name: "Drive",
        unit: "dB",
        min: 0.0,
        max: 24.0,
        default: 6.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "ceiling_db",
        name: "Ceiling",
        unit: "dB",
        min: -12.0,
        max: 0.0,
        default: -1.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "knee",
        name: "Knee",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 50.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "mode",
        name: "Mode",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        scale: ParamScale::Stepped {
            labels: &MODE_LABELS,
        },
        decimals: 0,
    },
];

/// Soft Clipper paramétrico $C^2$ continuo (`linear` hasta `a = C - K`, asíntota exacta `C`) con ADAA1 en `f64`.
#[derive(Debug, Clone, Copy)]
struct ParametricSoftClipAdaa1 {
    ceiling: f64,
    knee_width: f64,
    prev_x: f64,
    prev_f1: f64,
}

impl ParametricSoftClipAdaa1 {
    fn new(ceiling: f64, knee_pct: f64) -> Self {
        let mut s = Self {
            ceiling: 0.89,
            knee_width: 0.4,
            prev_x: 0.0,
            prev_f1: 0.0,
        };
        s.set_curve(ceiling, knee_pct);
        s
    }

    #[inline]
    fn set_curve(&mut self, ceiling: f64, knee_pct: f64) {
        let c = ceiling.clamp(0.1, 2.0);
        let k = c * (0.05 + 0.85 * (knee_pct * 0.01).clamp(0.0, 1.0));
        if (c - self.ceiling).abs() > 1e-7 || (k - self.knee_width).abs() > 1e-7 {
            self.ceiling = c;
            self.knee_width = k;
            self.prev_f1 = Self::antiderivative(self.prev_x, c, k);
        }
    }

    fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_f1 = 0.0;
    }

    #[inline]
    fn shape(x: f64, c: f64, k: f64) -> f64 {
        let a = (c - k).max(0.0);
        let ax = x.abs();
        if ax <= a {
            x
        } else {
            let u = (ax - a) / k.max(1e-6);
            x.signum() * (a + k * u.tanh())
        }
    }

    #[inline]
    fn antiderivative(x: f64, c: f64, k: f64) -> f64 {
        let a = (c - k).max(0.0);
        let ax = x.abs();
        if ax <= a {
            0.5 * x * x
        } else {
            let u = (ax - a) / k.max(1e-6);
            0.5 * a * a + a * (ax - a) + k * k * ln_cosh_stable(u)
        }
    }

    #[inline]
    fn process_sample(&mut self, x: f64) -> f64 {
        let c = self.ceiling;
        let k = self.knee_width;
        let f1 = Self::antiderivative(x, c, k);
        let dx = x - self.prev_x;
        let out = if dx.abs() < 1e-6 {
            Self::shape(0.5 * (x + self.prev_x), c, k)
        } else {
            (f1 - self.prev_f1) / dx
        };
        self.prev_x = x;
        self.prev_f1 = f1;
        out
    }
}

/// Pedal dual `Soft Clip / Tape` con `Oversampler` + `ADAA1` y pre/de-énfasis en agudos.
pub struct SoftClipTape {
    drive_db: f32,
    ceiling_db: f32,
    knee_pct: f32,
    mode: usize,
    sample_rate: f32,
    max_block: usize,

    drive_smoother: OnePoleSmoother,
    mode_cf: ModeCrossfader,

    os_clip: Oversampler,
    soft_clip: ParametricSoftClipAdaa1,

    pre_emph: MatchedBiquad,
    os_tape: Oversampler,
    tape_clip: AsymSoftClipAdaa1,
    tape_dc: DcBlocker,
    de_emph: MatchedBiquad,
    last_emph_db: f32,

    scratch_clip: Vec<f32>,
    scratch_tape: Vec<f32>,
}

impl Default for SoftClipTape {
    fn default() -> Self {
        Self::new()
    }
}

impl SoftClipTape {
    pub fn new() -> Self {
        let c0 = db_to_lin(-1.0) as f64;
        let mut s = Self {
            drive_db: 6.0,
            ceiling_db: -1.0,
            knee_pct: 50.0,
            mode: 0,
            sample_rate: 48_000.0,
            max_block: 512,
            drive_smoother: OnePoleSmoother::new(10.0, 48_000.0, db_to_lin(6.0)),
            mode_cf: ModeCrossfader::new(6.0, 48_000.0, 0),
            os_clip: Oversampler::new(48_000.0, 512),
            soft_clip: ParametricSoftClipAdaa1::new(c0, 50.0),
            pre_emph: MatchedBiquad::new(48_000.0),
            os_tape: Oversampler::new(48_000.0, 512),
            tape_clip: AsymSoftClipAdaa1::new(c0, 0.92 * c0),
            tape_dc: DcBlocker::new(10.0),
            de_emph: MatchedBiquad::new(48_000.0),
            last_emph_db: -999.0,
            scratch_clip: vec![0.0; 512],
            scratch_tape: vec![0.0; 512],
        };
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_drive_db(&mut self, db: f32) {
        let val = TAPE_LIMITER_PARAMS[0].clamp(db);
        self.drive_db = val;
        self.drive_smoother.set_target(db_to_lin(val));
    }

    pub fn set_ceiling_db(&mut self, db: f32) {
        let val = TAPE_LIMITER_PARAMS[1].clamp(db);
        self.ceiling_db = val;
        let c = 10.0_f64.powf(val as f64 * 0.05);
        self.soft_clip.set_curve(c, self.knee_pct as f64);
        self.tape_clip.set_limits(c, 0.92 * c);
    }

    pub fn set_knee_pct(&mut self, pct: f32) {
        let val = TAPE_LIMITER_PARAMS[2].clamp(pct);
        self.knee_pct = val;
        let c = 10.0_f64.powf(self.ceiling_db as f64 * 0.05);
        self.soft_clip.set_curve(c, val as f64);
        self.update_emphasis(val);
    }

    pub fn set_mode(&mut self, val: f32) {
        let idx = TAPE_LIMITER_PARAMS[3].step_index(val);
        self.mode = idx;
        self.mode_cf.set_mode(idx);
    }

    fn update_emphasis(&mut self, knee_pct: f32) {
        let emph_db = 2.0 + 6.0 * (knee_pct * 0.01).clamp(0.0, 1.0);
        if (emph_db - self.last_emph_db).abs() > 1e-4 {
            self.last_emph_db = emph_db;
            self.pre_emph
                .set_params(MatchedFilterKind::HighShelf, 3_000.0, 0.707, emph_db);
            self.de_emph
                .set_params(MatchedFilterKind::HighShelf, 3_000.0, 0.707, -emph_db);
        }
    }
}

impl Effect for SoftClipTape {
    fn kind(&self) -> &'static str {
        "soft_clipper"
    }

    fn name(&self) -> &'static str {
        "Soft Clip / Tape"
    }

    fn accent_color(&self) -> [u8; 3] {
        [16, 185, 129]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &TAPE_LIMITER_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.drive_db,
            1 => self.ceiling_db,
            2 => self.knee_pct,
            3 => self.mode as f32,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_drive_db(value),
            1 => self.set_ceiling_db(value),
            2 => self.set_knee_pct(value),
            3 => self.set_mode(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        let sr = sample_rate.max(1.0);
        let blk = max_block.max(1);
        self.sample_rate = sr;
        self.max_block = blk;

        self.drive_smoother.set_sample_rate(sr);
        self.mode_cf.set_sample_rate(sr);

        self.os_clip.prepare(sr, blk);
        self.os_tape.prepare(sr, blk);
        self.pre_emph.set_sample_rate(sr);
        self.de_emph.set_sample_rate(sr);
        self.tape_dc.prepare(sr);

        self.scratch_clip.resize(blk, 0.0);
        self.scratch_tape.resize(blk, 0.0);
        self.last_emph_db = -999.0;
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let chunk_cap = self.max_block.max(1);
        for chunk in buf.chunks_mut(chunk_cap) {
            let n = chunk.len();
            for ((dst, sc_c), sc_t) in chunk
                .iter()
                .zip(self.scratch_clip[..n].iter_mut())
                .zip(self.scratch_tape[..n].iter_mut())
            {
                let x = sanitize_sample(*dst);
                let driven = x * self.drive_smoother.next_val();
                *sc_c = driven;
                *sc_t = self.pre_emph.process_sample(driven);
            }

            let soft_clip = &mut self.soft_clip;
            self.os_clip
                .process_block(&mut self.scratch_clip[..n], |os_buf, _| {
                    for s in os_buf.iter_mut() {
                        *s = soft_clip.process_sample(*s);
                    }
                });

            let tape_clip = &mut self.tape_clip;
            self.os_tape
                .process_block(&mut self.scratch_tape[..n], |os_buf, _| {
                    for s in os_buf.iter_mut() {
                        *s = tape_clip.process_sample(*s);
                    }
                });

            for ((dst, &y_clip), &y_tape_raw) in chunk
                .iter_mut()
                .zip(self.scratch_clip[..n].iter())
                .zip(self.scratch_tape[..n].iter())
            {
                let y_tape_dc = self.tape_dc.process_sample(y_tape_raw);
                let y_tape = self.de_emph.process_sample(y_tape_dc);

                let (prev_m, next_m) = self.mode_cf.pair();
                let (g_prev, g_next) = self.mode_cf.advance_gains();
                let a = if prev_m == 0 { y_clip } else { y_tape };
                let b = if next_m == 0 { y_clip } else { y_tape };
                *dst = a * g_prev + b * g_next;
            }
        }
    }

    fn reset(&mut self) {
        self.drive_smoother.reset(db_to_lin(self.drive_db));
        self.mode_cf.reset(self.mode);
        self.os_clip.reset();
        self.soft_clip.reset();
        self.os_tape.reset();
        self.tape_clip.reset();
        self.pre_emph.reset();
        self.tape_dc.reset();
        self.de_emph.reset();

        let c = 10.0_f64.powf(self.ceiling_db as f64 * 0.05);
        self.soft_clip.set_curve(c, self.knee_pct as f64);
        self.tape_clip.set_limits(c, 0.92 * c);
        self.last_emph_db = -999.0;
        self.update_emphasis(self.knee_pct);
    }
}
