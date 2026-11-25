//! Compresor Óptico estilo **Teletronix LA-2A / celda T4B** (`D10`):
//! - Topología *feedback* con latencia nula, estado en `f64` y computador *soft-knee* de
//!   Giannoulis et al. (JAES 2012).
//! - Celda fotorresistora CdS en dos etapas (recuperación rápida ~`55 ms` + cola lenta
//!   `0.6–3.5 s` modulada por un integrador de historial de exposición `H`).
//! - Controles clásicos: `Peak Red.`, `Gain`, selector `Stepped` (`Compress` / `Limit` con
//!   crossfade `8 ms`) y mezcla paralela `Mix`.

use crate::denormals::flush_denormal_f64;
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const MODE_LABELS: [&str; 2] = ["Compress", "Limit"];

const OPTO_PARAMS: [ParamSpec; 4] = [
    ParamSpec {
        id: "peak_reduction",
        name: "Peak Red.",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 40.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "gain_db",
        name: "Gain",
        unit: "dB",
        min: -6.0,
        max: 24.0,
        default: 4.0,
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

/// Atenuación positiva en dB según la ecuación exacta de codo suave de Giannoulis et al. (2012).
#[inline]
fn soft_knee_gr(x_db: f64, thresh_db: f64, ratio: f64, width_db: f64) -> f64 {
    let diff = x_db - thresh_db;
    let half_w = 0.5 * width_db;
    let y_db = if 2.0 * diff < -width_db {
        x_db
    } else if 2.0 * diff.abs() <= width_db {
        let t = diff + half_w;
        x_db + (1.0 / ratio - 1.0) * (t * t) / (2.0 * width_db)
    } else {
        thresh_db + diff / ratio
    };
    (x_db - y_db).max(0.0)
}

/// Compresor Óptico con celda T4B dual-stage y topología *feedback*.
pub struct OptoCompressor {
    peak_reduction: f32,
    gain_db: f32,
    mode: usize,
    mix_pct: f32,
    sample_rate: f64,

    pr_smoother: OnePoleSmoother,
    gain_smoother: OnePoleSmoother,
    mix_smoother: OnePoleSmoother,
    mode_cf: ModeCrossfader,

    rms_alpha: f64,
    cds_att_coeff: f64,
    cds_fast_rel_coeff: f64,
    mem_charge_coeff: f64,
    mem_discharge_coeff: f64,

    y_fb: f64,
    rms_power: f64,
    fast_gr: f64,
    slow_gr: f64,
    gr_total: f64,
    history_h: f64,
}

impl Default for OptoCompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl OptoCompressor {
    pub fn new() -> Self {
        let mut s = Self {
            peak_reduction: 40.0,
            gain_db: 4.0,
            mode: 0,
            mix_pct: 100.0,
            sample_rate: 48_000.0,
            pr_smoother: OnePoleSmoother::new(10.0, 48_000.0, 40.0),
            gain_smoother: OnePoleSmoother::new(10.0, 48_000.0, db_to_lin(4.0)),
            mix_smoother: OnePoleSmoother::new(10.0, 48_000.0, 1.0),
            mode_cf: ModeCrossfader::new(8.0, 48_000.0, 0),
            rms_alpha: 0.0,
            cds_att_coeff: 0.0,
            cds_fast_rel_coeff: 0.0,
            mem_charge_coeff: 0.0,
            mem_discharge_coeff: 0.0,
            y_fb: 0.0,
            rms_power: 0.0,
            fast_gr: 0.0,
            slow_gr: 0.0,
            gr_total: 0.0,
            history_h: 0.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_peak_reduction(&mut self, pct: f32) {
        let val = OPTO_PARAMS[0].clamp(pct);
        self.peak_reduction = val;
        self.pr_smoother.set_target(val);
    }

    pub fn set_gain_db(&mut self, db: f32) {
        let val = OPTO_PARAMS[1].clamp(db);
        self.gain_db = val;
        self.gain_smoother.set_target(db_to_lin(val));
    }

    pub fn set_mode(&mut self, val: f32) {
        let idx = OPTO_PARAMS[2].step_index(val);
        self.mode = idx;
        self.mode_cf.set_mode(idx);
    }

    pub fn set_mix(&mut self, pct: f32) {
        let val = OPTO_PARAMS[3].clamp(pct);
        self.mix_pct = val;
        self.mix_smoother.set_target(val * 0.01);
    }

    /// Reducción de ganancia instantánea en dB positivos (`>= 0.0`).
    #[inline]
    pub fn gain_reduction_db(&self) -> f32 {
        self.gr_total as f32
    }
}

impl Effect for OptoCompressor {
    fn kind(&self) -> &'static str {
        "opto_comp"
    }

    fn name(&self) -> &'static str {
        "Opto Comp"
    }

    fn accent_color(&self) -> [u8; 3] {
        [168, 85, 247]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &OPTO_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.peak_reduction,
            1 => self.gain_db,
            2 => self.mode as f32,
            3 => self.mix_pct,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_peak_reduction(value),
            1 => self.set_gain_db(value),
            2 => self.set_mode(value),
            3 => self.set_mix(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = (sample_rate as f64).max(1.0);
        self.sample_rate = sr;
        let sr32 = sr as f32;

        self.pr_smoother.set_sample_rate(sr32);
        self.gain_smoother.set_sample_rate(sr32);
        self.mix_smoother.set_sample_rate(sr32);
        self.mode_cf.set_sample_rate(sr32);

        self.rms_alpha = 1.0 - (-1.0 / (0.030 * sr)).exp();
        self.cds_att_coeff = (-1.0 / (0.010 * sr)).exp();
        self.cds_fast_rel_coeff = (-1.0 / (0.055 * sr)).exp();
        self.mem_charge_coeff = (-1.0 / (3.0 * sr)).exp();
        self.mem_discharge_coeff = (-1.0 / (8.0 * sr)).exp();

        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = sanitize_sample(*s) as f64;
            let pr = self.pr_smoother.next_f64();
            let makeup = self.gain_smoother.next_f64();
            let mix = self.mix_smoother.next_f64();

            // 1. Detector RMS en realimentación (feedback) sobre la salida comprimida previa.
            self.rms_power = flush_denormal_f64(
                self.rms_power + (self.y_fb * self.y_fb - self.rms_power) * self.rms_alpha,
            );
            let level_db = 10.0 * (2.0 * self.rms_power + 1.0e-12).log10();

            // 2. Computador estático soft-knee: Compress (R=3, W=12) vs Limit (R=12, W=6).
            let thresh_db = 6.0 - 0.5 * pr;
            let gr_comp = soft_knee_gr(level_db, thresh_db, 3.0, 12.0);
            let gr_lim = soft_knee_gr(level_db, thresh_db, 12.0, 6.0);

            let (prev_m, next_m) = self.mode_cf.pair();
            let (g_prev, g_next) = self.mode_cf.advance_gains();
            let a = if prev_m == 0 { gr_comp } else { gr_lim };
            let b = if next_m == 0 { gr_comp } else { gr_lim };
            let gr_target = a * (g_prev as f64) + b * (g_next as f64);

            // 3. Celda CdS dual-stage con memoria H modulando la cola lenta (0.6 s .. 3.5 s).
            if gr_target > self.fast_gr {
                self.fast_gr = gr_target + self.cds_att_coeff * (self.fast_gr - gr_target);
            } else {
                self.fast_gr = gr_target + self.cds_fast_rel_coeff * (self.fast_gr - gr_target);
            }

            let tau_slow = 0.6 + 2.9 * self.history_h;
            let slow_rel_coeff = (-1.0 / (tau_slow * self.sample_rate)).exp();
            if gr_target > self.slow_gr {
                self.slow_gr = gr_target + self.cds_att_coeff * (self.slow_gr - gr_target);
            } else {
                self.slow_gr = gr_target + slow_rel_coeff * (self.slow_gr - gr_target);
            }

            self.gr_total = flush_denormal_f64(0.55 * self.fast_gr + 0.45 * self.slow_gr);

            // 4. Acumulador de historial de iluminación H.
            let u = (self.gr_total / 10.0).min(1.0);
            let mem_coeff = if u > self.history_h {
                self.mem_charge_coeff
            } else {
                self.mem_discharge_coeff
            };
            self.history_h = flush_denormal_f64(u + mem_coeff * (self.history_h - u));

            // 5. Aplicación de ganancia, realimentación y mezcla paralela + makeup.
            let g_lin = 10.0_f64.powf(-self.gr_total * 0.05);
            let y_comp = x * g_lin;
            self.y_fb = y_comp;

            let y_out = (mix * y_comp + (1.0 - mix) * x) * makeup;
            *s = y_out as f32;
        }
    }

    fn reset(&mut self) {
        self.y_fb = 0.0;
        self.rms_power = 0.0;
        self.fast_gr = 0.0;
        self.slow_gr = 0.0;
        self.gr_total = 0.0;
        self.history_h = 0.0;
        self.pr_smoother.reset(self.peak_reduction);
        self.gain_smoother.reset(db_to_lin(self.gain_db));
        self.mix_smoother.reset(self.mix_pct * 0.01);
        self.mode_cf.reset(self.mode);
    }
}
