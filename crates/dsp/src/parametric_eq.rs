//! Ecualizador Paramétrico de 4 bandas sin *Nyquist cramping* (`D13` Opción B).
//!
//! Usa filtros *matched* de Vicanek (2016) / Orfanidis (1997) ([`MatchedBiquad`]) en todo el
//! rango de frecuencias de muestreo (`44.1–192 kHz`):
//! - Banda 1: `Low Shelf` (`40–400 Hz`, `±15 dB`)
//! - Bandas 2 y 3: `Peaking / Bell` (`100 Hz–8 kHz`, `±15 dB`, `Q = 0.4..8.0`)
//! - Banda 4: `High Shelf` (`1.5–12 kHz`, `±15 dB`)
//! - Control `Level` (`±12 dB`) suavizado por muestra sin *zipper noise*.

use crate::matched_eq::{MatchedBiquad, MatchedFilterKind};
use crate::smoother::OnePoleSmoother;
use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const SHELF_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;
const SMOOTH_TAU_MS: f32 = 10.0;
const COEFF_UPDATE_SUBBLOCK: usize = 16;

const EQ_PARAMS: [ParamSpec; 11] = [
    ParamSpec {
        id: "low_freq_hz",
        name: "Low Freq",
        unit: "Hz",
        min: 40.0,
        max: 400.0,
        default: 100.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "low_gain_db",
        name: "Low Gain",
        unit: "dB",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "lm_freq_hz",
        name: "LM Freq",
        unit: "Hz",
        min: 100.0,
        max: 8_000.0,
        default: 400.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "lm_gain_db",
        name: "LM Gain",
        unit: "dB",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "lm_q",
        name: "LM Q",
        unit: "",
        min: 0.4,
        max: 8.0,
        default: 1.0,
        scale: ParamScale::Logarithmic,
        decimals: 2,
    },
    ParamSpec {
        id: "hm_freq_hz",
        name: "HM Freq",
        unit: "Hz",
        min: 100.0,
        max: 8_000.0,
        default: 2_200.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "hm_gain_db",
        name: "HM Gain",
        unit: "dB",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "hm_q",
        name: "HM Q",
        unit: "",
        min: 0.4,
        max: 8.0,
        default: 1.2,
        scale: ParamScale::Logarithmic,
        decimals: 2,
    },
    ParamSpec {
        id: "high_freq_hz",
        name: "High Freq",
        unit: "Hz",
        min: 1_500.0,
        max: 12_000.0,
        default: 5_500.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "high_gain_db",
        name: "High Gain",
        unit: "dB",
        min: -15.0,
        max: 15.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
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

/// Ecualizador Paramétrico de 4 bandas (*Low Shelf*, *LM Bell*, *HM Bell*, *High Shelf*) + `Level`.
pub struct ParametricEq {
    low_freq_hz: f32,
    low_gain_db: f32,
    lm_freq_hz: f32,
    lm_gain_db: f32,
    lm_q: f32,
    hm_freq_hz: f32,
    hm_gain_db: f32,
    hm_q: f32,
    high_freq_hz: f32,
    high_gain_db: f32,
    level_db: f32,

    low_shelf: MatchedBiquad,
    lm_bell: MatchedBiquad,
    hm_bell: MatchedBiquad,
    high_shelf: MatchedBiquad,

    low_freq_s: OnePoleSmoother,
    low_gain_s: OnePoleSmoother,
    lm_freq_s: OnePoleSmoother,
    lm_gain_s: OnePoleSmoother,
    lm_q_s: OnePoleSmoother,
    hm_freq_s: OnePoleSmoother,
    hm_gain_s: OnePoleSmoother,
    hm_q_s: OnePoleSmoother,
    high_freq_s: OnePoleSmoother,
    high_gain_s: OnePoleSmoother,
    level_s: OnePoleSmoother,

    coeffs_dirty: bool,
    sub_counter: usize,
}

impl Default for ParametricEq {
    fn default() -> Self {
        Self::new()
    }
}

impl ParametricEq {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let mut eq = Self {
            low_freq_hz: EQ_PARAMS[0].default,
            low_gain_db: EQ_PARAMS[1].default,
            lm_freq_hz: EQ_PARAMS[2].default,
            lm_gain_db: EQ_PARAMS[3].default,
            lm_q: EQ_PARAMS[4].default,
            hm_freq_hz: EQ_PARAMS[5].default,
            hm_gain_db: EQ_PARAMS[6].default,
            hm_q: EQ_PARAMS[7].default,
            high_freq_hz: EQ_PARAMS[8].default,
            high_gain_db: EQ_PARAMS[9].default,
            level_db: EQ_PARAMS[10].default,
            low_shelf: MatchedBiquad::new(sr),
            lm_bell: MatchedBiquad::new(sr),
            hm_bell: MatchedBiquad::new(sr),
            high_shelf: MatchedBiquad::new(sr),
            low_freq_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[0].default),
            low_gain_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[1].default),
            lm_freq_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[2].default),
            lm_gain_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[3].default),
            lm_q_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[4].default),
            hm_freq_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[5].default),
            hm_gain_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[6].default),
            hm_q_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[7].default),
            high_freq_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[8].default),
            high_gain_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, EQ_PARAMS[9].default),
            level_s: OnePoleSmoother::new(SMOOTH_TAU_MS, sr, db_to_lin(EQ_PARAMS[10].default)),
            coeffs_dirty: false,
            sub_counter: 0,
        };
        eq.prepare(sr, 512);
        eq
    }

    pub fn set_low_band(&mut self, freq_hz: f32, gain_db: f32) {
        self.set_param(0, freq_hz);
        self.set_param(1, gain_db);
    }

    pub fn set_low_mid_band(&mut self, freq_hz: f32, gain_db: f32, q: f32) {
        self.set_param(2, freq_hz);
        self.set_param(3, gain_db);
        self.set_param(4, q);
    }

    pub fn set_high_mid_band(&mut self, freq_hz: f32, gain_db: f32, q: f32) {
        self.set_param(5, freq_hz);
        self.set_param(6, gain_db);
        self.set_param(7, q);
    }

    pub fn set_high_band(&mut self, freq_hz: f32, gain_db: f32) {
        self.set_param(8, freq_hz);
        self.set_param(9, gain_db);
    }

    pub fn set_level_db(&mut self, level_db: f32) {
        self.set_param(10, level_db);
    }

    #[inline]
    fn apply_exact_filter_params(&mut self, use_smoothed: bool) {
        let (lf, lg, lmf, lmg, lmq, hmf, hmg, hmq, hf, hg) = if use_smoothed {
            (
                self.low_freq_s.current(),
                self.low_gain_s.current(),
                self.lm_freq_s.current(),
                self.lm_gain_s.current(),
                self.lm_q_s.current(),
                self.hm_freq_s.current(),
                self.hm_gain_s.current(),
                self.hm_q_s.current(),
                self.high_freq_s.current(),
                self.high_gain_s.current(),
            )
        } else {
            (
                self.low_freq_hz,
                self.low_gain_db,
                self.lm_freq_hz,
                self.lm_gain_db,
                self.lm_q,
                self.hm_freq_hz,
                self.hm_gain_db,
                self.hm_q,
                self.high_freq_hz,
                self.high_gain_db,
            )
        };
        self.low_shelf
            .set_params(MatchedFilterKind::LowShelf, lf, SHELF_Q, lg);
        self.lm_bell
            .set_params(MatchedFilterKind::Peaking, lmf, lmq, lmg);
        self.hm_bell
            .set_params(MatchedFilterKind::Peaking, hmf, hmq, hmg);
        self.high_shelf
            .set_params(MatchedFilterKind::HighShelf, hf, SHELF_Q, hg);
    }

    #[inline]
    fn any_band_unsettled(&self) -> bool {
        !self.low_freq_s.is_settled()
            || !self.low_gain_s.is_settled()
            || !self.lm_freq_s.is_settled()
            || !self.lm_gain_s.is_settled()
            || !self.lm_q_s.is_settled()
            || !self.hm_freq_s.is_settled()
            || !self.hm_gain_s.is_settled()
            || !self.hm_q_s.is_settled()
            || !self.high_freq_s.is_settled()
            || !self.high_gain_s.is_settled()
    }

    #[inline]
    fn step_band_smoothers(&mut self) {
        self.low_freq_s.next_f64();
        self.low_gain_s.next_f64();
        self.lm_freq_s.next_f64();
        self.lm_gain_s.next_f64();
        self.lm_q_s.next_f64();
        self.hm_freq_s.next_f64();
        self.hm_gain_s.next_f64();
        self.hm_q_s.next_f64();
        self.high_freq_s.next_f64();
        self.high_gain_s.next_f64();
    }

    /// Magnitud analítica combinada de las 4 bandas + `Level` en dB a `freq_hz`.
    pub fn magnitude_db_at(&self, freq_hz: f32) -> f32 {
        let g_lin = self.low_shelf.magnitude_response(freq_hz) as f64
            * self.lm_bell.magnitude_response(freq_hz) as f64
            * self.hm_bell.magnitude_response(freq_hz) as f64
            * self.high_shelf.magnitude_response(freq_hz) as f64
            * db_to_lin(self.level_db) as f64;
        crate::lin_to_db(g_lin as f32)
    }
}

impl Effect for ParametricEq {
    fn kind(&self) -> &'static str {
        "parametric_eq"
    }

    fn name(&self) -> &'static str {
        "Parametric EQ"
    }

    fn accent_color(&self) -> [u8; 3] {
        [56, 189, 248]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &EQ_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.low_freq_hz,
            1 => self.low_gain_db,
            2 => self.lm_freq_hz,
            3 => self.lm_gain_db,
            4 => self.lm_q,
            5 => self.hm_freq_hz,
            6 => self.hm_gain_db,
            7 => self.hm_q,
            8 => self.high_freq_hz,
            9 => self.high_gain_db,
            10 => self.level_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = EQ_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                if (self.low_freq_hz - v).abs() > 1e-4 {
                    self.low_freq_hz = v;
                    self.low_freq_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            1 => {
                if (self.low_gain_db - v).abs() > 1e-4 {
                    self.low_gain_db = v;
                    self.low_gain_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            2 => {
                if (self.lm_freq_hz - v).abs() > 1e-4 {
                    self.lm_freq_hz = v;
                    self.lm_freq_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            3 => {
                if (self.lm_gain_db - v).abs() > 1e-4 {
                    self.lm_gain_db = v;
                    self.lm_gain_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            4 => {
                if (self.lm_q - v).abs() > 1e-4 {
                    self.lm_q = v;
                    self.lm_q_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            5 => {
                if (self.hm_freq_hz - v).abs() > 1e-4 {
                    self.hm_freq_hz = v;
                    self.hm_freq_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            6 => {
                if (self.hm_gain_db - v).abs() > 1e-4 {
                    self.hm_gain_db = v;
                    self.hm_gain_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            7 => {
                if (self.hm_q - v).abs() > 1e-4 {
                    self.hm_q = v;
                    self.hm_q_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            8 => {
                if (self.high_freq_hz - v).abs() > 1e-4 {
                    self.high_freq_hz = v;
                    self.high_freq_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            9 => {
                if (self.high_gain_db - v).abs() > 1e-4 {
                    self.high_gain_db = v;
                    self.high_gain_s.set_target(v);
                    self.coeffs_dirty = true;
                }
            }
            10 => {
                self.level_db = v;
                self.level_s.set_target(db_to_lin(v));
            }
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.low_shelf.set_sample_rate(sr);
        self.lm_bell.set_sample_rate(sr);
        self.hm_bell.set_sample_rate(sr);
        self.high_shelf.set_sample_rate(sr);

        self.low_freq_s.set_sample_rate(sr);
        self.low_gain_s.set_sample_rate(sr);
        self.lm_freq_s.set_sample_rate(sr);
        self.lm_gain_s.set_sample_rate(sr);
        self.lm_q_s.set_sample_rate(sr);
        self.hm_freq_s.set_sample_rate(sr);
        self.hm_gain_s.set_sample_rate(sr);
        self.hm_q_s.set_sample_rate(sr);
        self.high_freq_s.set_sample_rate(sr);
        self.high_gain_s.set_sample_rate(sr);
        self.level_s.set_sample_rate(sr);

        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            if self.coeffs_dirty || self.any_band_unsettled() {
                self.step_band_smoothers();
                if self.sub_counter == 0 {
                    self.apply_exact_filter_params(true);
                }
                self.sub_counter = (self.sub_counter + 1) % COEFF_UPDATE_SUBBLOCK;
                if !self.any_band_unsettled() {
                    self.apply_exact_filter_params(false);
                    self.coeffs_dirty = false;
                    self.sub_counter = 0;
                }
            }

            let x = sanitize_sample(*s) as f64;
            let y1 = self.low_shelf.process_sample_f64(x);
            let y2 = self.lm_bell.process_sample_f64(y1);
            let y3 = self.hm_bell.process_sample_f64(y2);
            let y4 = self.high_shelf.process_sample_f64(y3);
            let lvl = self.level_s.next_f64();
            *s = (y4 * lvl) as f32;
        }
    }

    fn reset(&mut self) {
        self.low_freq_s.reset(self.low_freq_hz);
        self.low_gain_s.reset(self.low_gain_db);
        self.lm_freq_s.reset(self.lm_freq_hz);
        self.lm_gain_s.reset(self.lm_gain_db);
        self.lm_q_s.reset(self.lm_q);
        self.hm_freq_s.reset(self.hm_freq_hz);
        self.hm_gain_s.reset(self.hm_gain_db);
        self.hm_q_s.reset(self.hm_q);
        self.high_freq_s.reset(self.high_freq_hz);
        self.high_gain_s.reset(self.high_gain_db);
        self.level_s.reset(db_to_lin(self.level_db));

        self.apply_exact_filter_params(false);
        self.low_shelf.reset();
        self.lm_bell.reset();
        self.hm_bell.reset();
        self.high_shelf.reset();
        self.coeffs_dirty = false;
        self.sub_counter = 0;
    }
}
