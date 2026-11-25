//! Clean Boost / Preamp (`D8`):
//! - 3 controles: `Gain` (`0..+26 dB`), `Mode` (`Stepped`: `Op-Amp` vs `JFET` con crossfade `5 ms`)
//!   y `Tilt` (`-6..+6 dB` con pivote en `1 kHz`).
//! - Internos fijos verificados: filtro subsónico `SvfTpt` pasa-altos a `30 Hz` (`< 0.1 dB` en
//!   `82.41 Hz`) y techo suave `ADAA1` equivalente a alimentación `18 V` (`Op-Amp` lineal con
//!   `THD < -100 dB` en operación normal; `JFET` con `H2` musical vía asimetría suave).

use crate::adaa::{ln_cosh_stable, AsymSoftClipAdaa1};
use crate::matched_eq::{MatchedBiquad, MatchedFilterKind};
use crate::oversampler::Oversampler;
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::svf::{SvfMode, SvfTpt};
use crate::{db_to_lin, sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const MODE_LABELS: [&str; 2] = ["Op-Amp", "JFET"];

const CLEAN_BOOST_PARAMS: [ParamSpec; 3] = [
    ParamSpec {
        id: "gain_db",
        name: "Gain",
        unit: "dB",
        min: 0.0,
        max: 26.0,
        default: 6.0,
        scale: ParamScale::Linear,
        decimals: 1,
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
    ParamSpec {
        id: "tilt_db",
        name: "Tilt",
        unit: "dB",
        min: -6.0,
        max: 6.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
];

/// Techo lineal estricto (`K = 1.5` ~ `+3.5 dBFS`) con hombro suave C1 hasta riel de `18 V` (`2.0`)
/// procesado mediante ADAA1 en `f64` (`THD = 0` cuando `|x| <= K`).
#[derive(Debug, Clone, Copy, Default)]
struct OpAmpRailAdaa1 {
    prev_x: f64,
    prev_f1: f64,
}

impl OpAmpRailAdaa1 {
    const KNEE: f64 = 1.50;
    const SHOULDER: f64 = 0.50; // KNEE + SHOULDER = 2.0 (+6.02 dBFS, headroom equivalente 18 V)

    fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_f1 = 0.0;
    }

    #[inline]
    fn shape(x: f64) -> f64 {
        let ax = x.abs();
        if ax <= Self::KNEE {
            x
        } else {
            let u = (ax - Self::KNEE) / Self::SHOULDER;
            x.signum() * (Self::KNEE + Self::SHOULDER * u.tanh())
        }
    }

    #[inline]
    fn antiderivative(x: f64) -> f64 {
        let ax = x.abs();
        if ax <= Self::KNEE {
            0.5 * x * x
        } else {
            let u = (ax - Self::KNEE) / Self::SHOULDER;
            0.5 * Self::KNEE * Self::KNEE
                + Self::KNEE * (ax - Self::KNEE)
                + Self::SHOULDER * Self::SHOULDER * ln_cosh_stable(u)
        }
    }

    #[inline]
    fn process_sample(&mut self, x: f64) -> f64 {
        let f1 = Self::antiderivative(x);
        let dx = x - self.prev_x;
        let out = if dx.abs() < 1e-6 {
            Self::shape(0.5 * (x + self.prev_x))
        } else {
            (f1 - self.prev_f1) / dx
        };
        self.prev_x = x;
        self.prev_f1 = f1;
        out
    }
}

/// Pedal `Clean Boost / Preamp` (`Op-Amp` transparente de `18 V` / `JFET` cálido con Tilt EQ).
pub struct CleanBoost {
    gain_db: f32,
    mode: usize,
    tilt_db: f32,
    sample_rate: f32,

    gain_smoother: OnePoleSmoother,
    tilt_smoother: OnePoleSmoother,
    mode_cf: ModeCrossfader,

    subsonic_hp: SvfTpt,
    tilt_low: MatchedBiquad,
    tilt_high: MatchedBiquad,
    last_tilt_applied: f32,

    oversampler: Oversampler,
    opamp_adaa: OpAmpRailAdaa1,
    jfet_adaa: AsymSoftClipAdaa1,
    jfet_dc: DcBlocker,
    jfet_lp: SvfTpt,
}

impl Default for CleanBoost {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanBoost {
    pub fn new() -> Self {
        let mut s = Self {
            gain_db: 6.0,
            mode: 0,
            tilt_db: 0.0,
            sample_rate: 48_000.0,
            gain_smoother: OnePoleSmoother::new(8.0, 48_000.0, db_to_lin(6.0)),
            tilt_smoother: OnePoleSmoother::new(10.0, 48_000.0, 0.0),
            mode_cf: ModeCrossfader::new(5.0, 48_000.0, 0),
            subsonic_hp: SvfTpt::new(48_000.0),
            tilt_low: MatchedBiquad::new(48_000.0),
            tilt_high: MatchedBiquad::new(48_000.0),
            last_tilt_applied: 999.0,
            oversampler: Oversampler::new(48_000.0, 512),
            opamp_adaa: OpAmpRailAdaa1::default(),
            jfet_adaa: AsymSoftClipAdaa1::new(2.2, 1.15),
            jfet_dc: DcBlocker::new(15.0),
            jfet_lp: SvfTpt::new(48_000.0),
        };
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_gain_db(&mut self, gain_db: f32) {
        let db = CLEAN_BOOST_PARAMS[0].clamp(gain_db);
        self.gain_db = db;
        self.gain_smoother.set_target(db_to_lin(db));
    }

    pub fn set_mode(&mut self, val: f32) {
        let idx = CLEAN_BOOST_PARAMS[1].step_index(val);
        self.mode = idx;
        self.mode_cf.set_mode(idx);
    }

    pub fn set_tilt_db(&mut self, tilt_db: f32) {
        let db = CLEAN_BOOST_PARAMS[2].clamp(tilt_db);
        self.tilt_db = db;
        self.tilt_smoother.set_target(db);
    }

    #[inline]
    fn update_tilt_filters(&mut self, tilt: f32) {
        if (tilt - self.last_tilt_applied).abs() <= 1e-4 {
            return;
        }
        self.last_tilt_applied = tilt;
        self.tilt_low
            .set_params(MatchedFilterKind::LowShelf, 1_000.0, 0.65, -tilt);
        self.tilt_high
            .set_params(MatchedFilterKind::HighShelf, 1_000.0, 0.65, tilt);
    }
}

impl Effect for CleanBoost {
    fn kind(&self) -> &'static str {
        "clean_boost"
    }

    fn name(&self) -> &'static str {
        "Clean Boost"
    }

    fn accent_color(&self) -> [u8; 3] {
        [56, 189, 248]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &CLEAN_BOOST_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.gain_db,
            1 => self.mode as f32,
            2 => self.tilt_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_gain_db(value),
            1 => self.set_mode(value),
            2 => self.set_tilt_db(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.oversampler.prepare(self.sample_rate, max_block.max(1));
        let os_sr = self.oversampler.oversampled_sample_rate();

        self.subsonic_hp.set_sample_rate(self.sample_rate);
        self.subsonic_hp.set_params(
            SvfMode::HighPass,
            30.0,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );

        self.tilt_low.set_sample_rate(self.sample_rate);
        self.tilt_high.set_sample_rate(self.sample_rate);
        self.tilt_smoother.set_sample_rate(self.sample_rate);
        self.tilt_smoother.reset(self.tilt_db);
        self.last_tilt_applied = 999.0;
        self.update_tilt_filters(self.tilt_db);

        self.gain_smoother.set_sample_rate(os_sr);
        self.gain_smoother.reset(db_to_lin(self.gain_db));
        self.mode_cf.set_sample_rate(os_sr);
        self.mode_cf.reset(self.mode);

        self.jfet_dc.prepare(os_sr);
        self.jfet_lp.set_sample_rate(os_sr);
        self.jfet_lp.set_params(
            SvfMode::LowPass,
            14_000.0_f32.min(0.42 * os_sr),
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let clean = self.subsonic_hp.process_sample(sanitize_sample(*s));
            let tilt = self.tilt_smoother.next_val();
            self.update_tilt_filters(tilt);
            let t1 = self.tilt_low.process_sample(clean);
            *s = self.tilt_high.process_sample(t1);
        }

        let gain_smoother = &mut self.gain_smoother;
        let mode_cf = &mut self.mode_cf;
        let opamp_adaa = &mut self.opamp_adaa;
        let jfet_adaa = &mut self.jfet_adaa;
        let jfet_dc = &mut self.jfet_dc;
        let jfet_lp = &mut self.jfet_lp;

        self.oversampler.process_block(buf, |os_buf, _| {
            for s in os_buf.iter_mut() {
                let g = gain_smoother.next_f64();
                let x = (*s * g).clamp(-64.0, 64.0);

                // Evaluamos siempre ambas ramas para transición sin saltos ni clics.
                let y_op = opamp_adaa.process_sample(x);
                let y_jfet_raw = jfet_adaa.process_sample(x);
                let y_jfet_dc = jfet_dc.process_sample(y_jfet_raw as f32);
                let y_jfet = jfet_lp.process_sample_f64(y_jfet_dc as f64);

                let (prev_m, next_m) = mode_cf.pair();
                let (g_prev, g_next) = mode_cf.advance_gains();
                let a = if prev_m == 0 { y_op } else { y_jfet };
                let b = if next_m == 0 { y_op } else { y_jfet };
                *s = a * (g_prev as f64) + b * (g_next as f64);
            }
        });
    }

    fn reset(&mut self) {
        self.subsonic_hp.reset();
        self.tilt_low.reset();
        self.tilt_high.reset();
        self.oversampler.reset();
        self.opamp_adaa.reset();
        self.jfet_adaa.reset();
        self.jfet_dc.reset();
        self.jfet_lp.reset();
        self.gain_smoother.reset(db_to_lin(self.gain_db));
        self.tilt_smoother.reset(self.tilt_db);
        self.mode_cf.reset(self.mode);
    }
}
