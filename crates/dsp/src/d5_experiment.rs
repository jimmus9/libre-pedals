//! Experimento comparativo `D5` (Sección 3.5 de `docs/research/02_saturacion_y_overdrive.md`):
//! contraste reproducible entre **White-Box (WDF + Wright ω)** y **Gray-Box (ADAA1)** frente a
//! referencia implícita **Ground Truth (Newton-Raphson a 16× oversampling en `f64`)** sobre la
//! celda física de recorte del Ibanez TS-808.

use std::f64::consts::PI;
use std::time::Instant;

use crate::analyzer::{
    compute_esr_db, compute_harmonic_metrics, compute_imd_db, compute_spectrum_db_with_window,
    generate_coherent_sine, HarmonicMetrics, WindowKind,
};
use crate::oversampler::{adaptive_oversampling_factor, HalfBandStage2x};

// Constantes físicas de la etapa de clipping del TS-808 (Yeh et al., IEEE TASLP 2010).
const R1_OHM: f64 = 4_700.0;
const C1_FARAD: f64 = 0.047e-6;
const RF_BASE_OHM: f64 = 51_000.0;
const RF_POT_OHM: f64 = 500_000.0;
const CF_FARAD: f64 = 51.0e-12;
const DIODE_IS_AMP: f64 = 2.52e-9;
const DIODE_ETA: f64 = 1.752;
const THERMAL_VT_VOLT: f64 = 0.02585;
const V_TH: f64 = DIODE_ETA * THERMAL_VT_VOLT; // ~0.0452892 V

/// Función Omega de Wright `ω(z) = W(e^z)` (solución de `ω + ln(ω) = z`, ω > 0) sin exponenciales
/// en el lazo de audio principal salvo en la cola profunda `z <= -2`.
#[inline]
pub fn wright_omega(z: f64) -> f64 {
    if z <= -50.0 {
        return 0.0;
    }
    let mut w = if z <= -2.0 {
        let ez = z.exp();
        ez * (1.0 - ez + 1.5 * ez * ez)
    } else if z <= 2.0 {
        (0.35 * z + 0.5671432904097838).max(0.08)
    } else {
        let lnz = z.ln();
        z - lnz + lnz / z
    };

    // 3 pasos de Halley sobre f(w) = w + ln(w) - z llevan el residuo al piso de f64.
    for _ in 0..3 {
        w = w.max(1e-300);
        let r = w + w.ln() - z;
        let one_p_w = 1.0 + w;
        let denom = one_p_w * (1.0 + r / (2.0 * one_p_w * one_p_w));
        w -= (w * r) / denom;
    }
    w.max(0.0)
}

#[derive(Clone, Copy)]
struct Ts808LinearHp {
    g_eq1: f64,
    s_c1: f64,
}

impl Ts808LinearHp {
    fn new(effective_sr: f64) -> Self {
        let g_c1 = 2.0 * C1_FARAD * effective_sr;
        let g_eq1 = 1.0 / (R1_OHM + 1.0 / g_c1);
        Self { g_eq1, s_c1: 0.0 }
    }

    #[inline]
    fn step(&mut self, v_in: f64) -> f64 {
        let i1 = self.g_eq1 * (v_in - self.s_c1);
        let v_c1 = v_in - R1_OHM * i1;
        self.s_c1 = 2.0 * v_c1 - self.s_c1;
        i1
    }
}

/// Referencia exacta offline: 16× oversampling (4 etapas HIIR en `f64`) + regla trapezoidal implícita
/// resuelta por Newton-Raphson hasta `<= 1e-13 V`.
pub struct GroundTruth16xNr {
    stages: [HalfBandStage2x; 4],
    hp: Ts808LinearHp,
    rf: f64,
    g_cf: f64,
    g_p: f64,
    s_cf: f64,
}

impl GroundTruth16xNr {
    pub fn new(base_sr: f64, drive: f64) -> Self {
        let eff_sr = base_sr * 16.0;
        let rf = RF_BASE_OHM + drive.clamp(0.0, 1.0) * RF_POT_OHM;
        let g_cf = 2.0 * CF_FARAD * eff_sr;
        let g_p = g_cf + 1.0 / rf;
        Self {
            stages: [
                HalfBandStage2x::from_spec(10, 0.04),
                HalfBandStage2x::from_spec(6, 0.15),
                HalfBandStage2x::from_spec(4, 0.25),
                HalfBandStage2x::from_spec(3, 0.35),
            ],
            hp: Ts808LinearHp::new(eff_sr),
            rf,
            g_cf,
            g_p,
            s_cf: 0.0,
        }
    }

    #[inline]
    fn solve_vd(&self, i_eq: f64) -> f64 {
        // Arranque acotado analíticamente para garantizar convergencia monótona sin desbordamientos.
        let mut vd = if i_eq.abs() <= 1e-12 {
            0.0
        } else {
            let lin = i_eq / self.g_p;
            let log_bound =
                i_eq.signum() * V_TH * ((i_eq.abs() / (2.0 * DIODE_IS_AMP)) + 1.0).asinh();
            if lin.abs() < log_bound.abs() {
                lin
            } else {
                log_bound
            }
        };

        for _ in 0..30 {
            let arg = (vd / V_TH).clamp(-40.0, 40.0);
            let f = self.g_p * vd + 2.0 * DIODE_IS_AMP * arg.sinh() - i_eq;
            let df = self.g_p + (2.0 * DIODE_IS_AMP / V_TH) * arg.cosh();
            let step = f / df;
            vd -= step;
            if step.abs() <= 1e-13 {
                break;
            }
        }
        let _ = self.rf;
        vd
    }

    #[inline]
    fn step_oversampled(&mut self, v_in: f64) -> f64 {
        let i1 = self.hp.step(v_in);
        let i_eq = i1 + self.s_cf;
        let vd = self.solve_vd(i_eq);
        self.s_cf = 2.0 * self.g_cf * vd - self.s_cf;
        v_in + vd
    }

    pub fn process_block(&mut self, input: &[f32]) -> Vec<f32> {
        let mut out = Vec::with_capacity(input.len());
        for &s in input {
            let (e1, o1) = self.stages[0].upsample_sample(s as f64);
            let mut b16 = [0.0_f64; 16];
            let l1 = [e1, o1];
            for (i, &v1) in l1.iter().enumerate() {
                let (e2, o2) = self.stages[1].upsample_sample(v1);
                for (j, &v2) in [e2, o2].iter().enumerate() {
                    let (e3, o3) = self.stages[2].upsample_sample(v2);
                    for (k, &v3) in [e3, o3].iter().enumerate() {
                        let (e4, o4) = self.stages[3].upsample_sample(v3);
                        let base = i * 8 + j * 4 + k * 2;
                        b16[base] = self.step_oversampled(e4);
                        b16[base + 1] = self.step_oversampled(o4);
                    }
                }
            }
            let mut b8 = [0.0_f64; 8];
            for (i, slot) in b8.iter_mut().enumerate() {
                *slot = self.stages[3].downsample_pair(b16[2 * i], b16[2 * i + 1]);
            }
            let mut b4 = [0.0_f64; 4];
            for (i, slot) in b4.iter_mut().enumerate() {
                *slot = self.stages[2].downsample_pair(b8[2 * i], b8[2 * i + 1]);
            }
            let b2_0 = self.stages[1].downsample_pair(b4[0], b4[1]);
            let b2_1 = self.stages[1].downsample_pair(b4[2], b4[3]);
            let y = self.stages[0].downsample_pair(b2_0, b2_1);
            out.push(y as f32);
        }
        out
    }
}

/// Candidato White-Box: WDF / trapezoidal con raíz no lineal resuelta analíticamente vía **Wright ω**
/// bajo el factor adaptativo `M(fs)` (`4x` a 48 kHz, `1x` a 192 kHz).
pub struct WhiteBoxWdfWright {
    factor: usize,
    stage1: HalfBandStage2x,
    stage2: HalfBandStage2x,
    hp: Ts808LinearHp,
    g_cf: f64,
    inv_gp_vth: f64,
    ln_kappa: f64,
    s_cf: f64,
}

impl WhiteBoxWdfWright {
    pub fn new(base_sr: f64, drive: f64) -> Self {
        let factor = adaptive_oversampling_factor(base_sr as f32).min(4);
        let eff_sr = base_sr * factor as f64;
        let rf = RF_BASE_OHM + drive.clamp(0.0, 1.0) * RF_POT_OHM;
        let g_cf = 2.0 * CF_FARAD * eff_sr;
        let g_p = g_cf + 1.0 / rf;
        let gp_vth = g_p * V_TH;
        let kappa = DIODE_IS_AMP / gp_vth;
        Self {
            factor,
            stage1: HalfBandStage2x::from_spec(8, 0.045),
            stage2: HalfBandStage2x::from_spec(4, 0.25),
            hp: Ts808LinearHp::new(eff_sr),
            g_cf,
            inv_gp_vth: 1.0 / gp_vth,
            ln_kappa: kappa.ln(),
            s_cf: 0.0,
        }
    }

    #[inline]
    pub fn step_oversampled(&mut self, v_in: f64) -> f64 {
        let i1 = self.hp.step(v_in);
        let i_eq = i1 + self.s_cf;
        let a = i_eq * self.inv_gp_vth;
        // Formulación simétrica DAFx-19 de par antiparalelo con Wright ω (sin sinh/exp en zona activa).
        let v_tilde = a - wright_omega(self.ln_kappa + a) + wright_omega(self.ln_kappa - a);
        let vd = V_TH * v_tilde;
        self.s_cf = 2.0 * self.g_cf * vd - self.s_cf;
        v_in + vd
    }

    pub fn process_block(&mut self, input: &[f32], out: &mut [f32]) {
        match self.factor {
            1 => {
                for (dst, &s) in out.iter_mut().zip(input.iter()) {
                    *dst = self.step_oversampled(s as f64) as f32;
                }
            }
            2 => {
                for (dst, &s) in out.iter_mut().zip(input.iter()) {
                    let (e, o) = self.stage1.upsample_sample(s as f64);
                    let ye = self.step_oversampled(e);
                    let yo = self.step_oversampled(o);
                    *dst = self.stage1.downsample_pair(ye, yo) as f32;
                }
            }
            _ => {
                for (dst, &s) in out.iter_mut().zip(input.iter()) {
                    let (e1, o1) = self.stage1.upsample_sample(s as f64);
                    let (e2a, o2a) = self.stage2.upsample_sample(e1);
                    let (e2b, o2b) = self.stage2.upsample_sample(o1);
                    let y0 = self.step_oversampled(e2a);
                    let y1 = self.step_oversampled(o2a);
                    let y2 = self.step_oversampled(e2b);
                    let y3 = self.step_oversampled(o2b);
                    let d0 = self.stage2.downsample_pair(y0, y1);
                    let d1 = self.stage2.downsample_pair(y2, y3);
                    *dst = self.stage1.downsample_pair(d0, d1) as f32;
                }
            }
        }
    }
}

/// Tabla lineal por tramos con antiderivada cuadrática exacta para ADAA1 sobre `Vd(I1)` estático.
struct PiecewiseLinearAdaa1 {
    x_min: f64,
    x_max: f64,
    inv_dx: f64,
    dx: f64,
    ys: Vec<f64>,
    f1s: Vec<f64>,
    prev_x: f64,
    prev_f1: f64,
}

impl PiecewiseLinearAdaa1 {
    fn from_static_diode_curve(rf: f64, points: usize, i_max: f64) -> Self {
        let n = points.max(129);
        let x_min = -i_max;
        let x_max = i_max;
        let dx = (x_max - x_min) / (n - 1) as f64;
        let inv_dx = 1.0 / dx;

        let mut ys = Vec::with_capacity(n);
        for k in 0..n {
            let i_target = x_min + k as f64 * dx;
            let vd = solve_static_diode(i_target, rf);
            ys.push(vd);
        }

        let mut f1s = Vec::with_capacity(n);
        f1s.push(0.0);
        for k in 0..(n - 1) {
            let area = 0.5 * (ys[k] + ys[k + 1]) * dx;
            f1s.push(f1s[k] + area);
        }
        // Anclamos F1(0) = 0 para simetría numérica exacta.
        let mid_idx = (n - 1) / 2;
        let f_zero = f1s[mid_idx];
        for f in &mut f1s {
            *f -= f_zero;
        }

        Self {
            x_min,
            x_max,
            inv_dx,
            dx,
            ys,
            f1s,
            prev_x: 0.0,
            prev_f1: 0.0,
        }
    }

    #[inline]
    fn eval_y_and_f1(&self, x: f64) -> (f64, f64) {
        let n = self.ys.len();
        if x <= self.x_min {
            let dt = x - self.x_min;
            let slope = (self.ys[1] - self.ys[0]) * self.inv_dx;
            let y = self.ys[0] + slope * dt;
            let f1 = self.f1s[0] + self.ys[0] * dt + 0.5 * slope * dt * dt;
            return (y, f1);
        }
        if x >= self.x_max {
            let last = n - 1;
            let dt = x - self.x_max;
            let slope = (self.ys[last] - self.ys[last - 1]) * self.inv_dx;
            let y = self.ys[last] + slope * dt;
            let f1 = self.f1s[last] + self.ys[last] * dt + 0.5 * slope * dt * dt;
            return (y, f1);
        }

        let pos = (x - self.x_min) * self.inv_dx;
        let k = (pos.floor() as usize).min(n - 2);
        let t = x - (self.x_min + k as f64 * self.dx);
        let slope = (self.ys[k + 1] - self.ys[k]) * self.inv_dx;
        let y = self.ys[k] + slope * t;
        let f1 = self.f1s[k] + self.ys[k] * t + 0.5 * slope * t * t;
        (y, f1)
    }

    #[inline]
    fn process_sample(&mut self, x: f64) -> f64 {
        let (_, f1) = self.eval_y_and_f1(x);
        let dx = x - self.prev_x;
        let out = if dx.abs() < 1e-8 {
            self.eval_y_and_f1(0.5 * (x + self.prev_x)).0
        } else {
            (f1 - self.prev_f1) / dx
        };
        self.prev_x = x;
        self.prev_f1 = f1;
        out
    }
}

#[inline]
fn solve_static_diode(i1: f64, rf: f64) -> f64 {
    if i1.abs() < 1e-14 {
        return 0.0;
    }
    let g_rf = 1.0 / rf;
    let mut vd = {
        let lin = i1 * rf;
        let log_b = i1.signum() * V_TH * ((i1.abs() / (2.0 * DIODE_IS_AMP)) + 1.0).asinh();
        if lin.abs() < log_b.abs() {
            lin
        } else {
            log_b
        }
    };
    for _ in 0..30 {
        let arg = (vd / V_TH).clamp(-40.0, 40.0);
        let f = g_rf * vd + 2.0 * DIODE_IS_AMP * arg.sinh() - i1;
        let df = g_rf + (2.0 * DIODE_IS_AMP / V_TH) * arg.cosh();
        let step = f / df;
        vd -= step;
        if step.abs() < 1e-13 {
            break;
        }
    }
    vd
}

/// Candidato Gray-Box estructural: filtro lineal R1-C1 + `ADAA1` estático en LUT + rama directa
/// compensada en `0.5` muestras + filtro TPT de 1 polo calibrado al polo medio de `Cf = 51 pF`.
pub struct GrayBoxAdaa1 {
    factor: usize,
    stage1: HalfBandStage2x,
    stage2: HalfBandStage2x,
    hp: Ts808LinearHp,
    nl_adaa: PiecewiseLinearAdaa1,
    prev_vin: f64,
    cf_g: f64,
    cf_state: f64,
}

impl GrayBoxAdaa1 {
    pub fn new(base_sr: f64, drive: f64) -> Self {
        let factor = adaptive_oversampling_factor(base_sr as f32).min(4);
        let eff_sr = base_sr * factor as f64;
        let rf = RF_BASE_OHM + drive.clamp(0.0, 1.0) * RF_POT_OHM;
        // Polo RC medio efectivo de Cf = 51 pF en paralelo con la impedancia dinámica (~12 kHz).
        let fc_cf = 12_000.0_f64.min(0.45 * eff_sr);
        let cf_g = (PI * fc_cf / eff_sr).tan();
        Self {
            factor,
            stage1: HalfBandStage2x::from_spec(8, 0.045),
            stage2: HalfBandStage2x::from_spec(4, 0.25),
            hp: Ts808LinearHp::new(eff_sr),
            nl_adaa: PiecewiseLinearAdaa1::from_static_diode_curve(rf, 8193, 0.0008),
            prev_vin: 0.0,
            cf_g,
            cf_state: 0.0,
        }
    }

    #[inline]
    pub fn step_oversampled(&mut self, v_in: f64) -> f64 {
        let i1 = self.hp.step(v_in);
        let vd_nl = self.nl_adaa.process_sample(i1);
        // Integrador TPT de 1 polo para Cf.
        let v_lp = (self.cf_g * vd_nl + self.cf_state) / (1.0 + self.cf_g);
        self.cf_state = 2.0 * v_lp - self.cf_state;
        // Rama directa alineada exactamente 0.5 muestras para coincidir con el retardo ADAA1.
        let v_direct = 0.5 * (v_in + self.prev_vin);
        self.prev_vin = v_in;
        v_direct + v_lp
    }

    pub fn process_block(&mut self, input: &[f32], out: &mut [f32]) {
        match self.factor {
            1 => {
                for (dst, &s) in out.iter_mut().zip(input.iter()) {
                    *dst = self.step_oversampled(s as f64) as f32;
                }
            }
            2 => {
                for (dst, &s) in out.iter_mut().zip(input.iter()) {
                    let (e, o) = self.stage1.upsample_sample(s as f64);
                    let ye = self.step_oversampled(e);
                    let yo = self.step_oversampled(o);
                    *dst = self.stage1.downsample_pair(ye, yo) as f32;
                }
            }
            _ => {
                for (dst, &s) in out.iter_mut().zip(input.iter()) {
                    let (e1, o1) = self.stage1.upsample_sample(s as f64);
                    let (e2a, o2a) = self.stage2.upsample_sample(e1);
                    let (e2b, o2b) = self.stage2.upsample_sample(o1);
                    let y0 = self.step_oversampled(e2a);
                    let y1 = self.step_oversampled(o2a);
                    let y2 = self.step_oversampled(e2b);
                    let y3 = self.step_oversampled(o2b);
                    let d0 = self.stage2.downsample_pair(y0, y1);
                    let d1 = self.stage2.downsample_pair(y2, y3);
                    *dst = self.stage1.downsample_pair(d0, d1) as f32;
                }
            }
        }
    }
}

/// Métricas resumidas de un candidato para una tasa de muestreo dada.
#[derive(Debug, Clone)]
pub struct D5ModelRow {
    pub label: String,
    pub sample_rate: u32,
    pub esr_db_440: f32,
    pub thd_percent_440: f32,
    pub h2_db: f32,
    pub h3_db: f32,
    pub imd_fifth_db: f32,
    pub imd_third_db: f32,
    pub asnr_db_4500: f32,
    pub esr_db_transient: f32,
    pub ns_per_sample: f64,
    pub latency_samples: f32,
}

#[derive(Debug, Clone)]
pub struct D5SweepPoint {
    pub level_db: f32,
    pub drive_pct: f32,
    pub gt_thd_pct: f32,
    pub wb_thd_pct: f32,
    pub gb_thd_pct: f32,
    pub wb_esr_db: f32,
    pub gb_esr_db: f32,
}

/// Datos completos del experimento D5 listos para generar tablas y gráficas SVG.
pub struct D5ReportData {
    pub rows: Vec<D5ModelRow>,
    pub sweep: Vec<D5SweepPoint>,
    pub wave_time_ms: Vec<f32>,
    pub wave_gt: Vec<f32>,
    pub wave_wb: Vec<f32>,
    pub wave_gb: Vec<f32>,
    pub err_wb_mv: Vec<f32>,
    pub err_gb_mv: Vec<f32>,
    pub spec_freqs_48k: Vec<f32>,
    pub spec_gt_48k: Vec<f32>,
    pub spec_wb_48k: Vec<f32>,
    pub spec_gb_48k: Vec<f32>,
    pub spec_freqs_192k: Vec<f32>,
    pub spec_wb_192k: Vec<f32>,
    pub spec_gb_192k: Vec<f32>,
}

/// Interpola `sig` desplazada por un retardo fraccional real `delay` usando interpolación cúbica de Lagrange.
fn sample_cubic(sig: &[f32], pos: f64) -> f64 {
    let idx = pos.floor() as isize;
    if idx < 1 || (idx + 2) as usize >= sig.len() {
        let clamped = pos.round().clamp(0.0, (sig.len() - 1) as f64) as usize;
        return sig[clamped] as f64;
    }
    let u = pos - idx as f64;
    let i = idx as usize;
    let y_m1 = sig[i - 1] as f64;
    let y0 = sig[i] as f64;
    let y1 = sig[i + 1] as f64;
    let y2 = sig[i + 2] as f64;
    let c0 = y0;
    let c1 = (-2.0 * y_m1 - 3.0 * y0 + 6.0 * y1 - y2) / 6.0;
    let c2 = (y_m1 - 2.0 * y0 + y1) * 0.5;
    let c3 = (-y_m1 + 3.0 * y0 - 3.0 * y1 + y2) / 6.0;
    ((c3 * u + c2) * u + c1) * u + c0
}

/// Alinea `candidate` respecto a `reference` buscando el retardo fraccional óptimo en `[-6, +6]` muestras
/// y devuelve `(esr_db, lag_samples, reference_aligned, candidate_aligned)`.
pub fn aligned_esr_and_diff(
    reference: &[f32],
    candidate: &[f32],
    margin: usize,
) -> (f32, f32, Vec<f32>, Vec<f32>) {
    let end = reference.len().min(candidate.len()).saturating_sub(margin);
    if end <= margin + 16 {
        return (0.0, 0.0, Vec::new(), Vec::new());
    }
    let eval_slice = |lag: f64| -> f64 {
        let mut err_sq = 0.0_f64;
        let mut ref_sq = 0.0_f64;
        for (i, &ref_val) in reference.iter().enumerate().take(end).skip(margin) {
            let r = ref_val as f64;
            let c = sample_cubic(candidate, i as f64 + lag);
            let d = r - c;
            err_sq += d * d;
            ref_sq += r * r;
        }
        err_sq / ref_sq.max(1e-15)
    };

    // Búsqueda gruesa en pasos de 0.25 muestras seguida de refinamiento a 0.01 muestras.
    let mut best_lag = 0.0_f64;
    let mut best_ratio = f64::INFINITY;
    for step in -24..=24 {
        let lag = step as f64 * 0.25;
        let r = eval_slice(lag);
        if r < best_ratio {
            best_ratio = r;
            best_lag = lag;
        }
    }
    let center = best_lag;
    for step in -25..=25 {
        let lag = center + step as f64 * 0.01;
        let r = eval_slice(lag);
        if r < best_ratio {
            best_ratio = r;
            best_lag = lag;
        }
    }

    let mut ref_out = Vec::with_capacity(end - margin);
    let mut cand_out = Vec::with_capacity(end - margin);
    for (i, &ref_val) in reference.iter().enumerate().take(end).skip(margin) {
        ref_out.push(ref_val);
        cand_out.push(sample_cubic(candidate, i as f64 + best_lag) as f32);
    }
    let esr_db = compute_esr_db(&ref_out, &cand_out);
    (esr_db, best_lag as f32, ref_out, cand_out)
}

fn measure_latency_impulse(base_sr: u32, is_gray: bool, is_gt: bool) -> f32 {
    let mut imp = vec![0.0_f32; 256];
    imp[64] = 0.02;
    let mut out = vec![0.0_f32; 256];
    if is_gt {
        let mut gt = GroundTruth16xNr::new(base_sr as f64, 0.5);
        out = gt.process_block(&imp);
    } else if is_gray {
        let mut gb = GrayBoxAdaa1::new(base_sr as f64, 0.5);
        gb.process_block(&imp, &mut out);
    } else {
        let mut wb = WhiteBoxWdfWright::new(base_sr as f64, 0.5);
        wb.process_block(&imp, &mut out);
    }
    // Centroide de energía alrededor del pico de impulso.
    let (pk_i, _) = out
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
        .unwrap();
    let lo = pk_i.saturating_sub(4);
    let hi = (pk_i + 4).min(out.len() - 1);
    let mut w_sum = 0.0_f64;
    let mut p_sum = 0.0_f64;
    for (idx, &v) in out.iter().enumerate().take(hi + 1).skip(lo) {
        let p = (v as f64) * (v as f64);
        w_sum += idx as f64 * p;
        p_sum += p;
    }
    if p_sum > 1e-18 {
        ((w_sum / p_sum) - 64.0).max(0.0) as f32
    } else {
        0.0
    }
}

fn make_two_tone(f1: f32, f2: f32, sr: f32, len: usize, amp: f32) -> (Vec<f32>, f32, f32) {
    let c1 = ((f1 * len as f32) / sr).round().max(1.0);
    let c2 = ((f2 * len as f32) / sr).round().max(1.0);
    let f1_ex = c1 * sr / len as f32;
    let f2_ex = c2 * sr / len as f32;
    let mut v = Vec::with_capacity(len);
    for i in 0..len {
        let p1 = 2.0 * std::f32::consts::PI * c1 * i as f32 / len as f32;
        let p2 = 2.0 * std::f32::consts::PI * c2 * i as f32 / len as f32;
        v.push(0.5 * amp * (p1.sin() + p2.sin()));
    }
    (v, f1_ex, f2_ex)
}

fn make_guitar_pluck_pulse(sr: f32, len: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let t = i as f32 / sr;
        let attack = (t / 0.002).clamp(0.0, 1.0);
        let env = attack * (-t / 0.090).exp();
        let w = 2.0 * std::f32::consts::PI * 110.0 * t;
        let sig = 0.65 * w.sin() + 0.25 * (2.0 * w).sin() + 0.15 * (3.0 * w).sin();
        out.push(0.75 * env * sig);
    }
    out
}

type WaveBundle = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>);
type SpecBundle = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>);

fn eval_candidate_rate(
    sample_rate: u32,
    report_wave_and_spec: bool,
) -> (Vec<D5ModelRow>, Option<WaveBundle>, Option<SpecBundle>) {
    const WARMUP: usize = 2048;
    const N: usize = 8192;
    let sr = sample_rate as f32;
    let drive = 0.5_f64;

    // E1 (440 Hz @ -10 dBFS, Drive 50%)
    let amp_e1 = 10.0_f32.powf(-10.0 / 20.0);
    let (sig_e1, f_e1) = generate_coherent_sine(440.0, sr, WARMUP + N, amp_e1);

    let mut gt_e1 = GroundTruth16xNr::new(sample_rate as f64, drive);
    let y_gt_e1 = gt_e1.process_block(&sig_e1);

    let mut wb_e1 = WhiteBoxWdfWright::new(sample_rate as f64, drive);
    let mut y_wb_e1 = vec![0.0_f32; sig_e1.len()];
    wb_e1.process_block(&sig_e1, &mut y_wb_e1);

    let mut gb_e1 = GrayBoxAdaa1::new(sample_rate as f64, drive);
    let mut y_gb_e1 = vec![0.0_f32; sig_e1.len()];
    gb_e1.process_block(&sig_e1, &mut y_gb_e1);

    let m_gt_e1: HarmonicMetrics = compute_harmonic_metrics(
        &y_gt_e1[WARMUP..],
        sr,
        WindowKind::BlackmanHarris4,
        Some(f_e1),
    );
    let m_wb_e1 = compute_harmonic_metrics(
        &y_wb_e1[WARMUP..],
        sr,
        WindowKind::BlackmanHarris4,
        Some(f_e1),
    );
    let m_gb_e1 = compute_harmonic_metrics(
        &y_gb_e1[WARMUP..],
        sr,
        WindowKind::BlackmanHarris4,
        Some(f_e1),
    );

    let (esr_wb_e1, _, wave_gt, wave_wb) =
        aligned_esr_and_diff(&y_gt_e1[WARMUP..], &y_wb_e1[WARMUP..], 16);
    let (esr_gb_e1, _, _, wave_gb) =
        aligned_esr_and_diff(&y_gt_e1[WARMUP..], &y_gb_e1[WARMUP..], 16);

    // E2 (4.5 kHz @ -6 dBFS, Drive 75% -> aliasing)
    let amp_e2 = 10.0_f32.powf(-6.0 / 20.0);
    let (sig_e2, f_e2) = generate_coherent_sine(4_500.0, sr, WARMUP + N, amp_e2);
    let mut gt_e2 = GroundTruth16xNr::new(sample_rate as f64, 0.75);
    let y_gt_e2 = gt_e2.process_block(&sig_e2);
    let mut wb_e2 = WhiteBoxWdfWright::new(sample_rate as f64, 0.75);
    let mut y_wb_e2 = vec![0.0_f32; sig_e2.len()];
    wb_e2.process_block(&sig_e2, &mut y_wb_e2);
    let mut gb_e2 = GrayBoxAdaa1::new(sample_rate as f64, 0.75);
    let mut y_gb_e2 = vec![0.0_f32; sig_e2.len()];
    gb_e2.process_block(&sig_e2, &mut y_gb_e2);

    let m_gt_e2 = compute_harmonic_metrics(
        &y_gt_e2[WARMUP..],
        sr,
        WindowKind::BlackmanHarris4,
        Some(f_e2),
    );
    let m_wb_e2 = compute_harmonic_metrics(
        &y_wb_e2[WARMUP..],
        sr,
        WindowKind::BlackmanHarris4,
        Some(f_e2),
    );
    let m_gb_e2 = compute_harmonic_metrics(
        &y_gb_e2[WARMUP..],
        sr,
        WindowKind::BlackmanHarris4,
        Some(f_e2),
    );

    // E3a (quinta 220+330) y E3b (tercera mayor 220+277.18) @ -10 dBFS, Drive 50%
    let (sig_5, f5a, f5b) = make_two_tone(220.0, 330.0, sr, N, amp_e1);
    let mut in_5 = vec![0.0_f32; WARMUP];
    in_5.extend_from_slice(&sig_5);
    let y_gt_5 = GroundTruth16xNr::new(sample_rate as f64, drive).process_block(&in_5);
    let mut y_wb_5 = vec![0.0_f32; in_5.len()];
    WhiteBoxWdfWright::new(sample_rate as f64, drive).process_block(&in_5, &mut y_wb_5);
    let mut y_gb_5 = vec![0.0_f32; in_5.len()];
    GrayBoxAdaa1::new(sample_rate as f64, drive).process_block(&in_5, &mut y_gb_5);
    let imd5_gt = compute_imd_db(&y_gt_5[WARMUP..], sr, f5a, f5b);
    let imd5_wb = compute_imd_db(&y_wb_5[WARMUP..], sr, f5a, f5b);
    let imd5_gb = compute_imd_db(&y_gb_5[WARMUP..], sr, f5a, f5b);

    let (sig_3, f3a, f3b) = make_two_tone(220.0, 277.18, sr, N, amp_e1);
    let mut in_3 = vec![0.0_f32; WARMUP];
    in_3.extend_from_slice(&sig_3);
    let y_gt_3 = GroundTruth16xNr::new(sample_rate as f64, drive).process_block(&in_3);
    let mut y_wb_3 = vec![0.0_f32; in_3.len()];
    WhiteBoxWdfWright::new(sample_rate as f64, drive).process_block(&in_3, &mut y_wb_3);
    let mut y_gb_3 = vec![0.0_f32; in_3.len()];
    GrayBoxAdaa1::new(sample_rate as f64, drive).process_block(&in_3, &mut y_gb_3);
    let imd3_gt = compute_imd_db(&y_gt_3[WARMUP..], sr, f3a, f3b);
    let imd3_wb = compute_imd_db(&y_wb_3[WARMUP..], sr, f3a, f3b);
    let imd3_gb = compute_imd_db(&y_gb_3[WARMUP..], sr, f3a, f3b);

    // E4 (transitorio pulso de cuerda)
    let sig_e4 = make_guitar_pluck_pulse(sr, 4096);
    let y_gt_e4 = GroundTruth16xNr::new(sample_rate as f64, 0.5).process_block(&sig_e4);
    let mut y_wb_e4 = vec![0.0_f32; sig_e4.len()];
    WhiteBoxWdfWright::new(sample_rate as f64, 0.5).process_block(&sig_e4, &mut y_wb_e4);
    let mut y_gb_e4 = vec![0.0_f32; sig_e4.len()];
    GrayBoxAdaa1::new(sample_rate as f64, 0.5).process_block(&sig_e4, &mut y_gb_e4);
    let (esr_wb_e4, _, _, _) = aligned_esr_and_diff(&y_gt_e4, &y_wb_e4, 16);
    let (esr_gb_e4, _, _, _) = aligned_esr_and_diff(&y_gt_e4, &y_gb_e4, 16);

    // Benchmark CPU en nanosegundos / muestra (>= 240,000 muestras para candidatos; 48,000 para GT 16x).
    let bench_block = sig_e1[..4096].to_vec();
    let mut scratch = vec![0.0_f32; 4096];

    let gt_iters = 12_usize;
    let t_gt = Instant::now();
    let mut gt_bench = GroundTruth16xNr::new(sample_rate as f64, drive);
    for _ in 0..gt_iters {
        let res = gt_bench.process_block(std::hint::black_box(&bench_block));
        std::hint::black_box(res[0]);
    }
    let ns_gt = t_gt.elapsed().as_nanos() as f64 / (gt_iters * bench_block.len()) as f64;

    let cand_iters = 60_usize;
    let t_wb = Instant::now();
    let mut wb_bench = WhiteBoxWdfWright::new(sample_rate as f64, drive);
    for _ in 0..cand_iters {
        wb_bench.process_block(std::hint::black_box(&bench_block), &mut scratch);
        std::hint::black_box(scratch[0]);
    }
    let ns_wb = t_wb.elapsed().as_nanos() as f64 / (cand_iters * bench_block.len()) as f64;

    let t_gb = Instant::now();
    let mut gb_bench = GrayBoxAdaa1::new(sample_rate as f64, drive);
    for _ in 0..cand_iters {
        gb_bench.process_block(std::hint::black_box(&bench_block), &mut scratch);
        std::hint::black_box(scratch[0]);
    }
    let ns_gb = t_gb.elapsed().as_nanos() as f64 / (cand_iters * bench_block.len()) as f64;

    let lat_gt = measure_latency_impulse(sample_rate, false, true);
    let lat_wb = measure_latency_impulse(sample_rate, false, false);
    let lat_gb = measure_latency_impulse(sample_rate, true, false);

    let rows = vec![
        D5ModelRow {
            label: "Ground Truth 16x NR".to_string(),
            sample_rate,
            esr_db_440: -140.0,
            thd_percent_440: m_gt_e1.thd_percent,
            h2_db: m_gt_e1.h2_db,
            h3_db: m_gt_e1.h3_db,
            imd_fifth_db: imd5_gt,
            imd_third_db: imd3_gt,
            asnr_db_4500: m_gt_e2.aliasing_snr_db,
            esr_db_transient: -140.0,
            ns_per_sample: ns_gt,
            latency_samples: lat_gt,
        },
        D5ModelRow {
            label: "White-Box WDF + Wright ω".to_string(),
            sample_rate,
            esr_db_440: esr_wb_e1,
            thd_percent_440: m_wb_e1.thd_percent,
            h2_db: m_wb_e1.h2_db,
            h3_db: m_wb_e1.h3_db,
            imd_fifth_db: imd5_wb,
            imd_third_db: imd3_wb,
            asnr_db_4500: m_wb_e2.aliasing_snr_db,
            esr_db_transient: esr_wb_e4,
            ns_per_sample: ns_wb,
            latency_samples: lat_wb,
        },
        D5ModelRow {
            label: "Gray-Box + ADAA1".to_string(),
            sample_rate,
            esr_db_440: esr_gb_e1,
            thd_percent_440: m_gb_e1.thd_percent,
            h2_db: m_gb_e1.h2_db,
            h3_db: m_gb_e1.h3_db,
            imd_fifth_db: imd5_gb,
            imd_third_db: imd3_gb,
            asnr_db_4500: m_gb_e2.aliasing_snr_db,
            esr_db_transient: esr_gb_e4,
            ns_per_sample: ns_gb,
            latency_samples: lat_gb,
        },
    ];

    let spec_bins = N / 2;
    let mut s_gt = vec![-120.0_f32; spec_bins];
    let mut s_wb = vec![-120.0_f32; spec_bins];
    let mut s_gb = vec![-120.0_f32; spec_bins];
    compute_spectrum_db_with_window(&y_gt_e2[WARMUP..], &mut s_gt, WindowKind::BlackmanHarris4);
    compute_spectrum_db_with_window(&y_wb_e2[WARMUP..], &mut s_wb, WindowKind::BlackmanHarris4);
    compute_spectrum_db_with_window(&y_gb_e2[WARMUP..], &mut s_gb, WindowKind::BlackmanHarris4);
    let bin_hz = sr / N as f32;
    let freqs: Vec<f32> = (0..spec_bins).map(|k| k as f32 * bin_hz).collect();

    let wave_bundle = if report_wave_and_spec {
        // Tomamos ~2.5 ciclos (~5.7 ms = 274 muestras @ 48 kHz) en cruce ascendente.
        let zc = wave_gt
            .windows(2)
            .position(|w| w[0] < 0.0 && w[1] >= 0.0)
            .unwrap_or(0);
        let take = ((0.0057 * sr).round() as usize).min(wave_gt.len().saturating_sub(zc));
        let mut t_ms = Vec::with_capacity(take);
        let mut w_gt = Vec::with_capacity(take);
        let mut w_wb = Vec::with_capacity(take);
        let mut w_gb = Vec::with_capacity(take);
        let mut e_wb = Vec::with_capacity(take);
        let mut e_gb = Vec::with_capacity(take);
        for i in 0..take {
            let idx = zc + i;
            t_ms.push(i as f32 * 1000.0 / sr);
            let g = wave_gt[idx];
            let wb = wave_wb[idx];
            let gb = wave_gb[idx];
            w_gt.push(g);
            w_wb.push(wb);
            w_gb.push(gb);
            e_wb.push((wb - g) * 1000.0);
            e_gb.push((gb - g) * 1000.0);
        }
        Some((t_ms, w_gt, w_wb, w_gb, e_wb, e_gb))
    } else {
        None
    };

    (rows, wave_bundle, Some((freqs, s_gt, s_wb, s_gb)))
}

/// Ejecuta la batería completa del experimento `D5` (`48 kHz` y `192 kHz` + barrido de amplitud).
pub fn run_d5_experiment() -> D5ReportData {
    let (mut rows, wave_bundle, spec_48) = eval_candidate_rate(48_000, true);
    let (rows_192, _, spec_192) = eval_candidate_rate(192_000, false);
    rows.extend(rows_192);

    // Barrido de amplitud -40..0 dBFS para Drive 25%, 50%, 100% a 48 kHz.
    const WARMUP: usize = 1024;
    const N: usize = 4096;
    let sr = 48_000.0_f32;
    let mut sweep = Vec::new();
    for &drive_pct in &[25.0_f32, 50.0, 100.0] {
        let d = (drive_pct / 100.0) as f64;
        for &level_db in &[-40.0_f32, -30.0, -20.0, -10.0, -6.0, 0.0] {
            let amp = 10.0_f32.powf(level_db / 20.0);
            let (sig, f_ex) = generate_coherent_sine(440.0, sr, WARMUP + N, amp);
            let y_gt = GroundTruth16xNr::new(48_000.0, d).process_block(&sig);
            let mut y_wb = vec![0.0_f32; sig.len()];
            WhiteBoxWdfWright::new(48_000.0, d).process_block(&sig, &mut y_wb);
            let mut y_gb = vec![0.0_f32; sig.len()];
            GrayBoxAdaa1::new(48_000.0, d).process_block(&sig, &mut y_gb);

            let m_gt = compute_harmonic_metrics(
                &y_gt[WARMUP..],
                sr,
                WindowKind::BlackmanHarris4,
                Some(f_ex),
            );
            let m_wb = compute_harmonic_metrics(
                &y_wb[WARMUP..],
                sr,
                WindowKind::BlackmanHarris4,
                Some(f_ex),
            );
            let m_gb = compute_harmonic_metrics(
                &y_gb[WARMUP..],
                sr,
                WindowKind::BlackmanHarris4,
                Some(f_ex),
            );
            let (wb_esr_db, _, _, _) = aligned_esr_and_diff(&y_gt[WARMUP..], &y_wb[WARMUP..], 12);
            let (gb_esr_db, _, _, _) = aligned_esr_and_diff(&y_gt[WARMUP..], &y_gb[WARMUP..], 12);

            sweep.push(D5SweepPoint {
                level_db,
                drive_pct,
                gt_thd_pct: m_gt.thd_percent,
                wb_thd_pct: m_wb.thd_percent,
                gb_thd_pct: m_gb.thd_percent,
                wb_esr_db,
                gb_esr_db,
            });
        }
    }

    let (wave_time_ms, wave_gt, wave_wb, wave_gb, err_wb_mv, err_gb_mv) = wave_bundle.unwrap();
    let (spec_freqs_48k, spec_gt_48k, spec_wb_48k, spec_gb_48k) = spec_48.unwrap();
    let (spec_freqs_192k, _, spec_wb_192k, spec_gb_192k) = spec_192.unwrap();

    D5ReportData {
        rows,
        sweep,
        wave_time_ms,
        wave_gt,
        wave_wb,
        wave_gb,
        err_wb_mv,
        err_gb_mv,
        spec_freqs_48k,
        spec_gt_48k,
        spec_wb_48k,
        spec_gb_48k,
        spec_freqs_192k,
        spec_wb_192k,
        spec_gb_192k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wright_omega_identity_holds_across_domain() {
        for &z in &[-25.0, -5.0, -2.0, -0.5, 0.0, 0.5, 2.0, 5.0, 12.0, 40.0] {
            let w = wright_omega(z);
            assert!(w.is_finite() && w > 0.0, "bad w={w} for z={z}");
            let residual = (w + w.ln() - z).abs();
            assert!(residual < 1e-12, "wright_omega({z}) residual={residual}");
        }
    }

    #[test]
    fn d5_models_converge_and_match_ground_truth() {
        let sr = 48_000.0_f32;
        let (sig, f_ex) = generate_coherent_sine(440.0, sr, 2048 + 4096, 0.316);
        let y_gt = GroundTruth16xNr::new(48_000.0, 0.5).process_block(&sig);
        let mut y_wb = vec![0.0_f32; sig.len()];
        WhiteBoxWdfWright::new(48_000.0, 0.5).process_block(&sig, &mut y_wb);
        let mut y_gb = vec![0.0_f32; sig.len()];
        GrayBoxAdaa1::new(48_000.0, 0.5).process_block(&sig, &mut y_gb);

        let (esr_wb, _, _, _) = aligned_esr_and_diff(&y_gt[2048..], &y_wb[2048..], 16);
        let (esr_gb, _, _, _) = aligned_esr_and_diff(&y_gt[2048..], &y_gb[2048..], 16);
        let m_gt =
            compute_harmonic_metrics(&y_gt[2048..], sr, WindowKind::BlackmanHarris4, Some(f_ex));
        let m_wb =
            compute_harmonic_metrics(&y_wb[2048..], sr, WindowKind::BlackmanHarris4, Some(f_ex));
        let m_gb =
            compute_harmonic_metrics(&y_gb[2048..], sr, WindowKind::BlackmanHarris4, Some(f_ex));

        assert!(esr_wb < -35.0, "WhiteBox ESR vs GT unexpected: {esr_wb} dB");
        assert!(esr_gb < -25.0, "GrayBox ESR vs GT unexpected: {esr_gb} dB");
        assert!((m_gt.thd_percent - m_wb.thd_percent).abs() < 0.5);
        assert!((m_gt.thd_percent - m_gb.thd_percent).abs() < 1.5);
    }
}
