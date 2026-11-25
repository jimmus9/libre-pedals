//! Pedal **Reverb** multimodo (`Plate` / `Hall` / `Spring` — `D17` Opción A).
//!
//! Implementa tres topologías académicas de reverberación mono en tiempo real con `Pre-Delay` común
//! (`0–100 ms`), protección estricta contra *denormals* (`flush_denormal`) y conmutación sin clics:
//! 1. **Plate *(Jon Dattorro, JAES 1997)*:** 4 difusores *all-pass* de entrada seguidos por un tanque
//!    cruzado en figura-ocho con líneas moduladas y amortiguamiento de agudos.
//! 2. **Hall *(FDN ortogonal de 8 canales)*:** 8 líneas mutuamente primas acopladas mediante la
//!    matriz unitaria de Hadamard $H_8 / \sqrt{8}$ con tiempo de reverberación $RT_{60}$ físico.
//! 3. **Spring *(Välimäki, Parker & Abel, JAES 2010)*:** cascada dispersiva de 16 filtros *all-pass*
//!    de 2.º orden sobre [`SvfTpt`] en banda de `~3.65 kHz` acoplada a dos tanques de resorte,
//!    produciendo el característico *"drip / chirp"* elástico ante ataques de púa.

use std::f64::consts::TAU;

use crate::delay_line::{FractionalDelayLine, SchroederAllPass};
use crate::denormals::{flush_denormal, flush_denormal_f64};
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::svf::{SvfMode, SvfTpt};
use crate::{sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const REVERB_MODES: &[&str] = &["Plate", "Hall", "Spring"];

const REVERB_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "mode",
        name: "Mode",
        unit: "",
        min: 0.0,
        max: 2.0,
        default: 0.0,
        scale: ParamScale::Stepped {
            labels: REVERB_MODES,
        },
        decimals: 0,
    },
    ParamSpec {
        id: "decay",
        name: "Decay",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 52.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "predelay_ms",
        name: "Pre-Delay",
        unit: "ms",
        min: 0.0,
        max: 100.0,
        default: 15.0,
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
        id: "mix",
        name: "Mix",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 28.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
];

#[inline]
fn scale_dattorro(ref_samples: usize, sr: f32) -> usize {
    ((ref_samples as f64) * (sr as f64 / 29_761.0))
        .round()
        .max(2.0) as usize
}

#[inline]
fn scale_from_48k(ref_samples: usize, sr: f32) -> usize {
    ((ref_samples as f64) * (sr as f64 / 48_000.0))
        .round()
        .max(2.0) as usize
}

/// Tanque Plate de Dattorro (JAES 1997).
struct PlateTank {
    in_lp_state: f32,
    in_ap: [SchroederAllPass; 4],

    mod_ap_a: FractionalDelayLine,
    mod_ap_b: FractionalDelayLine,
    delay_a1: FractionalDelayLine,
    delay_a2: FractionalDelayLine,
    delay_b1: FractionalDelayLine,
    delay_b2: FractionalDelayLine,
    ap_a2: SchroederAllPass,
    ap_b2: SchroederAllPass,

    damp_a: f32,
    damp_b: f32,
    dc_a: DcBlocker,
    dc_b: DcBlocker,
    tank_a_out: f32,
    tank_b_out: f32,

    mod_a_base: f64,
    mod_b_base: f64,
    mod_excursion: f64,
    len_a1: usize,
    len_a2: usize,
    len_b1: usize,
    len_b2: usize,
    tap_a1: usize,
    tap_a2: usize,
    tap_b1: usize,
    tap_b2: usize,
    lfo_a: f64,
    lfo_b: f64,
}

impl PlateTank {
    fn new(sr: f32) -> Self {
        let mut t = Self {
            in_lp_state: 0.0,
            in_ap: [
                SchroederAllPass::new(),
                SchroederAllPass::new(),
                SchroederAllPass::new(),
                SchroederAllPass::new(),
            ],
            mod_ap_a: FractionalDelayLine::new(),
            mod_ap_b: FractionalDelayLine::new(),
            delay_a1: FractionalDelayLine::new(),
            delay_a2: FractionalDelayLine::new(),
            delay_b1: FractionalDelayLine::new(),
            delay_b2: FractionalDelayLine::new(),
            ap_a2: SchroederAllPass::new(),
            ap_b2: SchroederAllPass::new(),
            damp_a: 0.0,
            damp_b: 0.0,
            dc_a: DcBlocker::new(15.0),
            dc_b: DcBlocker::new(15.0),
            tank_a_out: 0.0,
            tank_b_out: 0.0,
            mod_a_base: 672.0,
            mod_b_base: 908.0,
            mod_excursion: 10.0,
            len_a1: 4453,
            len_a2: 3720,
            len_b1: 4217,
            len_b2: 3163,
            tap_a1: 266,
            tap_a2: 1990,
            tap_b1: 353,
            tap_b2: 1200,
            lfo_a: 0.0,
            lfo_b: 0.25,
        };
        t.prepare(sr);
        t
    }

    fn prepare(&mut self, sr: f32) {
        self.in_ap[0].prepare(scale_dattorro(142, sr), 0.75);
        self.in_ap[1].prepare(scale_dattorro(107, sr), 0.75);
        self.in_ap[2].prepare(scale_dattorro(379, sr), 0.625);
        self.in_ap[3].prepare(scale_dattorro(277, sr), 0.625);

        let exc = (10.0_f64 * (sr as f64 / 29_761.0)).clamp(2.0, 64.0);
        self.mod_excursion = exc;
        let mod_a = scale_dattorro(672, sr);
        let mod_b = scale_dattorro(908, sr);
        self.mod_a_base = (mod_a as f64).max(exc + 4.0);
        self.mod_b_base = (mod_b as f64).max(exc + 4.0);
        self.mod_ap_a
            .prepare((self.mod_a_base + exc).ceil() as usize + 8);
        self.mod_ap_b
            .prepare((self.mod_b_base + exc).ceil() as usize + 8);

        self.len_a1 = scale_dattorro(4453, sr);
        self.len_a2 = scale_dattorro(3720, sr);
        self.len_b1 = scale_dattorro(4217, sr);
        self.len_b2 = scale_dattorro(3163, sr);

        self.delay_a1.prepare(self.len_a1 + 8);
        self.delay_a2.prepare(self.len_a2 + 8);
        self.delay_b1.prepare(self.len_b1 + 8);
        self.delay_b2.prepare(self.len_b2 + 8);

        self.ap_a2.prepare(scale_dattorro(1800, sr), 0.5);
        self.ap_b2.prepare(scale_dattorro(2656, sr), 0.5);

        self.tap_a1 = scale_dattorro(266, sr).min(self.len_a1);
        self.tap_a2 = scale_dattorro(1990, sr).min(self.len_a2);
        self.tap_b1 = scale_dattorro(353, sr).min(self.len_b1);
        self.tap_b2 = scale_dattorro(1200, sr).min(self.len_b2);

        self.dc_a.prepare(sr);
        self.dc_b.prepare(sr);
        self.reset();
    }

    fn reset(&mut self) {
        self.in_lp_state = 0.0;
        for ap in &mut self.in_ap {
            ap.reset();
        }
        self.mod_ap_a.reset();
        self.mod_ap_b.reset();
        self.delay_a1.reset();
        self.delay_a2.reset();
        self.delay_b1.reset();
        self.delay_b2.reset();
        self.ap_a2.reset();
        self.ap_b2.reset();
        self.damp_a = 0.0;
        self.damp_b = 0.0;
        self.dc_a.reset();
        self.dc_b.reset();
        self.tank_a_out = 0.0;
        self.tank_b_out = 0.0;
        self.lfo_a = 0.0;
        self.lfo_b = 0.25;
    }

    #[inline]
    fn process_sample(&mut self, x: f32, decay_norm: f32, tone_norm: f32, sr: f64) -> f32 {
        // Ganancia de entrada y realimentación acotadas estrictamente < 1.0 (sin auto-oscilación).
        let bw_alpha = (-std::f64::consts::TAU * 8_500.0_f64.min(0.45 * sr) / sr).exp() as f32;
        self.in_lp_state = flush_denormal(x * (1.0 - bw_alpha) + bw_alpha * self.in_lp_state);

        let mut diffused = self.in_lp_state * 0.5;
        for ap in &mut self.in_ap {
            diffused = ap.process(diffused);
        }

        let decay = 0.28 + 0.60 * decay_norm.clamp(0.0, 1.0);
        let damp_fc = 1_800.0 + (tone_norm.clamp(0.0, 1.0) as f64) * 9_200.0;
        let damp_alpha = (-TAU * damp_fc.min(0.45 * sr) / sr).exp() as f32;

        let mod_a = self.mod_a_base + self.mod_excursion * (TAU * self.lfo_a).sin();
        let mod_b = self.mod_b_base + self.mod_excursion * (TAU * self.lfo_b).sin();
        self.lfo_a = (self.lfo_a + 0.77 / sr).fract();
        self.lfo_b = (self.lfo_b + 1.03 / sr).fract();

        // Rama A del tanque figura-ocho cruzado.
        let in_a = diffused + decay * self.tank_b_out;
        let d_mod_a = self.mod_ap_a.read_before_push_hermite(mod_a);
        let w_ap_a = flush_denormal((in_a + 0.70 * d_mod_a).clamp(-4.0, 4.0));
        self.mod_ap_a.push(w_ap_a);
        let ap_a1_out = d_mod_a - 0.70 * w_ap_a;

        let d_a1_end = self.delay_a1.read_before_push_int(self.len_a1);
        let d_a1_tap = self.delay_a1.read_before_push_int(self.tap_a1);
        self.delay_a1.push(ap_a1_out);

        self.damp_a = flush_denormal(d_a1_end * (1.0 - damp_alpha) + damp_alpha * self.damp_a);
        let ap_a2_out = self
            .ap_a2
            .process(self.dc_a.process_sample(self.damp_a) * decay);
        let d_a2_end = self.delay_a2.read_before_push_int(self.len_a2);
        let d_a2_tap = self.delay_a2.read_before_push_int(self.tap_a2);
        self.delay_a2.push(ap_a2_out);

        // Rama B del tanque figura-ocho cruzado.
        let in_b = diffused + decay * self.tank_a_out;
        let d_mod_b = self.mod_ap_b.read_before_push_hermite(mod_b);
        let w_ap_b = flush_denormal((in_b + 0.70 * d_mod_b).clamp(-4.0, 4.0));
        self.mod_ap_b.push(w_ap_b);
        let ap_b1_out = d_mod_b - 0.70 * w_ap_b;

        let d_b1_end = self.delay_b1.read_before_push_int(self.len_b1);
        let d_b1_tap = self.delay_b1.read_before_push_int(self.tap_b1);
        self.delay_b1.push(ap_b1_out);

        self.damp_b = flush_denormal(d_b1_end * (1.0 - damp_alpha) + damp_alpha * self.damp_b);
        let ap_b2_out = self
            .ap_b2
            .process(self.dc_b.process_sample(self.damp_b) * decay);
        let d_b2_end = self.delay_b2.read_before_push_int(self.len_b2);
        let d_b2_tap = self.delay_b2.read_before_push_int(self.tap_b2);
        self.delay_b2.push(ap_b2_out);

        self.tank_a_out = flush_denormal(d_a2_end);
        self.tank_b_out = flush_denormal(d_b2_end);

        0.42 * (d_a1_tap + d_b1_tap - d_a2_tap + d_b2_tap)
    }
}

/// FDN ortogonal de 8 canales con matriz de Hadamard $H_8 / \sqrt{8}$ para modo `Hall`.
struct HallTank {
    lines: [FractionalDelayLine; 8],
    lengths: [usize; 8],
    damp_state: [f32; 8],
    dc: [DcBlocker; 8],
    lfo_a: f64,
    lfo_b: f64,
    mod_exc: f64,
}

impl HallTank {
    const BASE_PRIMES_48K: [usize; 8] = [1499, 1777, 2053, 2377, 2713, 3083, 3499, 3967];

    fn new(sr: f32) -> Self {
        let mut h = Self {
            lines: std::array::from_fn(|_| FractionalDelayLine::new()),
            lengths: Self::BASE_PRIMES_48K,
            damp_state: [0.0; 8],
            dc: std::array::from_fn(|_| DcBlocker::new(15.0)),
            lfo_a: 0.0,
            lfo_b: 0.37,
            mod_exc: 12.0,
        };
        h.prepare(sr);
        h
    }

    fn prepare(&mut self, sr: f32) {
        self.mod_exc = (0.00025 * sr as f64).clamp(2.0, 48.0);
        for i in 0..8 {
            let len = scale_from_48k(Self::BASE_PRIMES_48K[i], sr).max((self.mod_exc as usize) + 8);
            self.lengths[i] = len;
            self.lines[i].prepare(len + (self.mod_exc as usize) + 16);
            self.dc[i].prepare(sr);
        }
        self.reset();
    }

    fn reset(&mut self) {
        for i in 0..8 {
            self.lines[i].reset();
            self.dc[i].reset();
        }
        self.damp_state = [0.0; 8];
        self.lfo_a = 0.0;
        self.lfo_b = 0.37;
    }

    #[inline]
    fn hadamard8(v: &mut [f32; 8]) {
        // Transformada rápida Walsh-Hadamard 8x8 normalizada por 1/sqrt(8).
        for i in (0..8).step_by(2) {
            let a = v[i];
            let b = v[i + 1];
            v[i] = a + b;
            v[i + 1] = a - b;
        }
        for i in (0..8).step_by(4) {
            for j in 0..2 {
                let a = v[i + j];
                let b = v[i + j + 2];
                v[i + j] = a + b;
                v[i + j + 2] = a - b;
            }
        }
        for j in 0..4 {
            let a = v[j];
            let b = v[j + 4];
            v[j] = a + b;
            v[j + 4] = a - b;
        }
        const INV_SQRT8: f32 = std::f32::consts::FRAC_1_SQRT_2 * 0.5;
        for val in v.iter_mut() {
            *val *= INV_SQRT8;
        }
    }

    #[inline]
    fn process_sample(&mut self, x: f32, decay_norm: f32, tone_norm: f32, sr: f64) -> f32 {
        let rt60 = 0.45 + 5.55 * (decay_norm.clamp(0.0, 1.0) as f64).powf(1.4);
        let damp_fc = 1_500.0 + (tone_norm.clamp(0.0, 1.0) as f64) * 9_000.0;
        let damp_alpha = (-TAU * damp_fc.min(0.45 * sr) / sr).exp() as f32;

        let m_a = self.mod_exc * (TAU * self.lfo_a).sin();
        let m_b = self.mod_exc * (TAU * self.lfo_b).sin();
        self.lfo_a = (self.lfo_a + 0.41 / sr).fract();
        self.lfo_b = (self.lfo_b + 0.57 / sr).fract();

        let mut d = [0.0_f32; 8];
        for i in 0..8 {
            let raw = match i {
                0 => self.lines[0].read_before_push_hermite(self.lengths[0] as f64 + m_a),
                4 => self.lines[4].read_before_push_hermite(self.lengths[4] as f64 + m_b),
                _ => self.lines[i].read_before_push_int(self.lengths[i]),
            };
            let clean = self.dc[i].process_sample(raw);
            self.damp_state[i] =
                flush_denormal(clean * (1.0 - damp_alpha) + damp_alpha * self.damp_state[i]);
            let delay_s = self.lengths[i] as f64 / sr;
            let g_i = 10.0_f64.powf(-3.0 * delay_s / rt60).min(0.985) as f32;
            d[i] = self.damp_state[i] * g_i;
        }

        let mut mixed = d;
        Self::hadamard8(&mut mixed);

        for i in 0..8 {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            self.lines[i].push(flush_denormal(mixed[i] + x * 0.35 * sign));
        }

        0.28 * ((d[0] + d[2] + d[4] + d[6]) - 0.7 * (d[1] + d[3] + d[5] + d[7]))
    }
}

/// Tanque Spring dispersivo de Välimäki et al. (JAES 2010): cascada all-pass sintonizada en ~3.65 kHz.
struct SpringTank {
    disp_ap: [SvfTpt; 16],
    delay_a: FractionalDelayLine,
    delay_b: FractionalDelayLine,
    len_a: usize,
    len_b: usize,
    mod_exc: f64,
    lfo: f64,

    loop_hp: SvfTpt,
    loop_lp: SvfTpt,
    dc: DcBlocker,
    fb_a: f64,
    fb_b: f64,
    sample_rate: f32,
}

impl SpringTank {
    fn new(sr: f32) -> Self {
        let mut s = Self {
            disp_ap: std::array::from_fn(|_| SvfTpt::new(sr)),
            delay_a: FractionalDelayLine::new(),
            delay_b: FractionalDelayLine::new(),
            len_a: 1790,
            len_b: 2480,
            mod_exc: 10.0,
            lfo: 0.0,
            loop_hp: SvfTpt::new(sr),
            loop_lp: SvfTpt::new(sr),
            dc: DcBlocker::new(25.0),
            fb_a: 0.0,
            fb_b: 0.0,
            sample_rate: sr,
        };
        s.prepare(sr);
        s
    }

    fn prepare(&mut self, sr: f32) {
        self.sample_rate = sr;
        let disp_fc = 3_650.0_f32.min(0.42 * sr);
        for ap in &mut self.disp_ap {
            ap.set_sample_rate(sr);
            ap.set_params(SvfMode::AllPass, disp_fc, 1.65, 0.0);
        }
        self.loop_hp.set_sample_rate(sr);
        self.loop_hp
            .set_params(SvfMode::HighPass, 110.0_f32.min(0.4 * sr), 0.707, 0.0);
        self.loop_lp.set_sample_rate(sr);
        self.dc.prepare(sr);

        self.mod_exc = (0.00025 * sr as f64).clamp(2.0, 48.0);
        self.len_a = ((0.0373 * sr).round() as usize).max((self.mod_exc as usize) + 8);
        self.len_b = ((0.0517 * sr).round() as usize).max((self.mod_exc as usize) + 8);
        self.delay_a
            .prepare(self.len_a + (self.mod_exc as usize) + 16);
        self.delay_b
            .prepare(self.len_b + (self.mod_exc as usize) + 16);
        self.reset();
    }

    fn reset(&mut self) {
        for ap in &mut self.disp_ap {
            ap.reset();
        }
        self.loop_hp.reset();
        self.loop_lp.reset();
        self.dc.reset();
        self.delay_a.reset();
        self.delay_b.reset();
        self.fb_a = 0.0;
        self.fb_b = 0.0;
        self.lfo = 0.0;
    }

    #[inline]
    fn process_sample(&mut self, x: f32, decay_norm: f32, tone_norm: f32, sr: f64) -> f32 {
        let lp_fc = (2_600.0 + tone_norm.clamp(0.0, 1.0) * 3_200.0).min(0.45 * self.sample_rate);
        self.loop_lp.set_params(SvfMode::LowPass, lp_fc, 0.707, 0.0);

        let mod_off = self.mod_exc * (TAU * self.lfo).sin();
        self.lfo = (self.lfo + 0.63 / sr).fract();

        let d_a = self
            .delay_a
            .read_before_push_hermite(self.len_a as f64 + mod_off);
        let d_b = self
            .delay_b
            .read_before_push_hermite(self.len_b as f64 - mod_off);

        let g_spring = (0.20 + 0.58 * decay_norm.clamp(0.0, 1.0)) as f64;
        let injected = (x as f64) * 0.55 + g_spring * (0.52 * self.fb_a - 0.48 * self.fb_b);

        // Cascada dispersiva de 16 etapas all-pass de 2.º orden -> retardo acentuado en 3–4.2 kHz ("chirp").
        let mut disp = injected;
        for ap in &mut self.disp_ap {
            disp = ap.process_sample_f64(disp);
        }

        let conditioned = self.loop_lp.process_sample_f64(
            self.loop_hp
                .process_sample_f64(self.dc.process_sample(disp as f32) as f64),
        );
        let saturated = (1.5 * (conditioned / 1.5).tanh()) as f32;

        self.delay_a.push(saturated);
        self.delay_b.push(-saturated * 0.92);

        self.fb_a = flush_denormal_f64(d_a as f64);
        self.fb_b = flush_denormal_f64(d_b as f64);

        0.65 * (0.55 * disp as f32 + 0.45 * (d_a + d_b))
    }
}

/// Reverb multimodo (`Plate`, `Hall`, `Spring`) con `Pre-Delay` y conmutación sin clics.
pub struct Reverb {
    mode: f32,
    decay: f32,
    predelay_ms: f32,
    tone: f32,
    mix: f32,
    sample_rate: f32,

    predelay_line: FractionalDelayLine,
    plate: PlateTank,
    hall: HallTank,
    spring: SpringTank,

    mode_xfade: ModeCrossfader,
    last_target_mode: usize,

    decay_s: OnePoleSmoother,
    predelay_s: OnePoleSmoother,
    tone_s: OnePoleSmoother,
    mix_s: OnePoleSmoother,
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Reverb {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let def_mode = REVERB_PARAMS[0].step_index(REVERB_PARAMS[0].default);
        let mut r = Self {
            mode: REVERB_PARAMS[0].default,
            decay: REVERB_PARAMS[1].default,
            predelay_ms: REVERB_PARAMS[2].default,
            tone: REVERB_PARAMS[3].default,
            mix: REVERB_PARAMS[4].default,
            sample_rate: sr,
            predelay_line: FractionalDelayLine::new(),
            plate: PlateTank::new(sr),
            hall: HallTank::new(sr),
            spring: SpringTank::new(sr),
            mode_xfade: ModeCrossfader::new(8.0, sr, def_mode),
            last_target_mode: def_mode,
            decay_s: OnePoleSmoother::new(15.0, sr, REVERB_PARAMS[1].default * 0.01),
            predelay_s: OnePoleSmoother::new(25.0, sr, REVERB_PARAMS[2].default),
            tone_s: OnePoleSmoother::new(15.0, sr, REVERB_PARAMS[3].default * 0.01),
            mix_s: OnePoleSmoother::new(10.0, sr, REVERB_PARAMS[4].default * 0.01),
        };
        r.prepare(sr, 512);
        r
    }

    pub fn set_mode(&mut self, mode: f32) {
        self.set_param(0, mode);
    }

    pub fn set_decay(&mut self, pct: f32) {
        self.set_param(1, pct);
    }

    pub fn set_predelay_ms(&mut self, ms: f32) {
        self.set_param(2, ms);
    }

    pub fn set_tone(&mut self, pct: f32) {
        self.set_param(3, pct);
    }

    pub fn set_mix(&mut self, pct: f32) {
        self.set_param(4, pct);
    }

    #[inline]
    fn process_tank(&mut self, mode_idx: usize, x: f32, decay_n: f32, tone_n: f32, sr: f64) -> f32 {
        match mode_idx {
            0 => self.plate.process_sample(x, decay_n, tone_n, sr),
            1 => self.hall.process_sample(x, decay_n, tone_n, sr),
            _ => self.spring.process_sample(x, decay_n, tone_n, sr),
        }
    }
}

impl Effect for Reverb {
    fn kind(&self) -> &'static str {
        "reverb"
    }

    fn name(&self) -> &'static str {
        "Reverb"
    }

    fn accent_color(&self) -> [u8; 3] {
        [139, 92, 246]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &REVERB_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.mode,
            1 => self.decay,
            2 => self.predelay_ms,
            3 => self.tone,
            4 => self.mix,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = REVERB_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                self.mode = v;
                let idx = spec.step_index(v);
                if idx != self.last_target_mode {
                    self.last_target_mode = idx;
                    match idx {
                        0 => self.plate.reset(),
                        1 => self.hall.reset(),
                        _ => self.spring.reset(),
                    }
                    self.mode_xfade.set_mode(idx);
                }
            }
            1 => {
                self.decay = v;
                self.decay_s.set_target(v * 0.01);
            }
            2 => {
                self.predelay_ms = v;
                self.predelay_s.set_target(v);
            }
            3 => {
                self.tone = v;
                self.tone_s.set_target(v * 0.01);
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
        let max_pre = (0.110 * sr).ceil() as usize + 16;
        self.predelay_line.prepare(max_pre);
        self.plate.prepare(sr);
        self.hall.prepare(sr);
        self.spring.prepare(sr);
        self.mode_xfade.set_sample_rate(sr);
        self.decay_s.set_sample_rate(sr);
        self.predelay_s.set_sample_rate(sr);
        self.tone_s.set_sample_rate(sr);
        self.mix_s.set_sample_rate(sr);
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let sr = (self.sample_rate as f64).max(1.0);
        let max_pre = (self.predelay_line.capacity().saturating_sub(6)) as f64;

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s);
            let decay_n = self.decay_s.next_val();
            let pre_ms = self.predelay_s.next_f64();
            let tone_n = self.tone_s.next_val();
            let mix = self.mix_s.next_val();

            self.predelay_line.push(x);
            let tank_in = if pre_ms < 0.05 {
                x
            } else {
                let d_samples = (pre_ms * 1e-3 * sr).clamp(2.0, max_pre);
                self.predelay_line.read_after_push_hermite(d_samples)
            };

            let xfade = self.mode_xfade.is_crossfading();
            let (prev_m, next_m) = self.mode_xfade.pair();
            let wet = if xfade && prev_m != next_m {
                let y_p = self.process_tank(prev_m, tank_in, decay_n, tone_n, sr);
                let y_n = self.process_tank(next_m, tank_in, decay_n, tone_n, sr);
                self.mode_xfade.mix_sample(y_p, y_n)
            } else {
                self.process_tank(next_m, tank_in, decay_n, tone_n, sr)
            };

            *s = x * (1.0 - mix) + wet * mix;
        }
    }

    fn reset(&mut self) {
        self.predelay_line.reset();
        self.plate.reset();
        self.hall.reset();
        self.spring.reset();
        let idx = REVERB_PARAMS[0].step_index(self.mode);
        self.last_target_mode = idx;
        self.mode_xfade.reset(idx);
        self.decay_s.reset(self.decay * 0.01);
        self.predelay_s.reset(self.predelay_ms);
        self.tone_s.reset(self.tone * 0.01);
        self.mix_s.reset(self.mix * 0.01);
    }
}
