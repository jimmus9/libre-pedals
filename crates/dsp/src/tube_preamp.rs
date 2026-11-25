//! Preamp de Válvula **12AX7** en cascada (`D7`):
//! - Curva física estática de placa-cátodo según **Norman Koren (1996)** tabulada una sola vez
//!   (`OnceLock` sin reservas en tiempo real) con **ADAA1-LUT** cuadrática en `f64`.
//! - Redes dinámicas por etapa: conducción de rejilla (*grid current* / *blocking sag*) +
//!   desplazamiento dinámico de cátodo (`Rk || Ck`), acoplo interetapa DC + capacitancia Miller e
//!   inversión de fase natural (`180°`) entre tríodos consecutivos.
//! - Selector `Stepped` sin clics (`ModeCrossfader` `6 ms`): `2x Crunch` vs `3x Lead`.

use std::sync::OnceLock;

use crate::matched_eq::{MatchedBiquad, MatchedFilterKind};
use crate::oversampler::Oversampler;
use crate::smoother::{ModeCrossfader, OnePoleSmoother};
use crate::svf::{SvfMode, SvfTpt};
use crate::{db_to_lin, sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const STAGES_LABELS: [&str; 2] = ["2x Crunch", "3x Lead"];

const TUBE_PREAMP_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "gain_db",
        name: "Gain",
        unit: "dB",
        min: 0.0,
        max: 42.0,
        default: 18.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "stages",
        name: "Stages",
        unit: "",
        min: 0.0,
        max: 1.0,
        default: 0.0,
        scale: ParamScale::Stepped {
            labels: &STAGES_LABELS,
        },
        decimals: 0,
    },
    ParamSpec {
        id: "bias",
        name: "Bias/Sag",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 45.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "tone_db",
        name: "Presence",
        unit: "dB",
        min: -8.0,
        max: 8.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "level_db",
        name: "Level",
        unit: "dB",
        min: -36.0,
        max: 12.0,
        default: -8.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
];

// Parámetros estándar Norman Koren (1996) para tríodo 12AX7 con carga de placa 100 kΩ @ 250 V.
const V_BB: f64 = 250.0;
const R_LOAD: f64 = 100_000.0;
const KOREN_MU: f64 = 100.0;
const KOREN_KP: f64 = 600.0;
const KOREN_KVB: f64 = 300.0;
const KOREN_KG1: f64 = 1060.0;
const KOREN_EX: f64 = 1.4;
const QUIESCENT_VGK: f64 = -1.45;
const GRID_VOLT_SCALE: f64 = 1.45;

#[inline]
fn koren_plate_current(v_pk: f64, v_gk: f64) -> f64 {
    if v_pk <= 0.0 {
        return 0.0;
    }
    let inner = KOREN_KP * (1.0 / KOREN_MU + v_gk / (KOREN_KVB + v_pk * v_pk).sqrt());
    let soft = if inner > 40.0 {
        inner
    } else if inner < -40.0 {
        0.0
    } else {
        inner.exp().ln_1p()
    };
    let e1 = (v_pk / KOREN_KP) * soft;
    if e1 <= 0.0 {
        0.0
    } else {
        e1.powf(KOREN_EX) / KOREN_KG1
    }
}

#[inline]
fn solve_plate_voltage(v_gk: f64) -> f64 {
    let mut lo = 0.0_f64;
    let mut hi = V_BB;
    for _ in 0..52 {
        let mid = 0.5 * (lo + hi);
        let i_tube = koren_plate_current(mid, v_gk);
        let i_load = (V_BB - mid) / R_LOAD;
        if i_tube > i_load {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    0.5 * (lo + hi)
}

/// Tabla precalculada de transferencia invertida de Koren (`12AX7`) con antiderivada exacta para ADAA1.
struct KorenTriodeLut {
    u_min: f64,
    u_max: f64,
    dx: f64,
    inv_dx: f64,
    ys: Vec<f64>,
    f1s: Vec<f64>,
}

impl KorenTriodeLut {
    fn build() -> Self {
        const POINTS: usize = 4097;
        let u_min = -4.0_f64;
        let u_max = 4.0_f64;
        let dx = (u_max - u_min) / (POINTS - 1) as f64;
        let inv_dx = 1.0 / dx;

        let v_pk_q = solve_plate_voltage(QUIESCENT_VGK);
        // Normalizamos la pendiente pequeña alrededor de QUIESCENT_VGK para ganancia unitaria controlada.
        let eps = 1e-3_f64;
        let slope_q = ((solve_plate_voltage(QUIESCENT_VGK - eps * GRID_VOLT_SCALE)
            - solve_plate_voltage(QUIESCENT_VGK + eps * GRID_VOLT_SCALE))
            / (2.0 * eps))
            .abs()
            .max(1.0);

        let mut ys = Vec::with_capacity(POINTS);
        for k in 0..POINTS {
            let u = u_min + k as f64 * dx;
            let v_gk = QUIESCENT_VGK + u * GRID_VOLT_SCALE;
            // Cada etapa tríodo en cátodo común invierte la fase 180°.
            let y = (solve_plate_voltage(v_gk) - v_pk_q) / slope_q;
            ys.push(y);
        }

        let mut f1s = Vec::with_capacity(POINTS);
        f1s.push(0.0);
        for k in 0..(POINTS - 1) {
            let area = 0.5 * (ys[k] + ys[k + 1]) * dx;
            f1s.push(f1s[k] + area);
        }
        let mid = (POINTS - 1) / 2;
        let f_zero = f1s[mid];
        for f in &mut f1s {
            *f -= f_zero;
        }

        Self {
            u_min,
            u_max,
            dx,
            inv_dx,
            ys,
            f1s,
        }
    }

    #[inline]
    fn eval(&self, x: f64) -> (f64, f64) {
        let n = self.ys.len();
        if x <= self.u_min {
            let dt = x - self.u_min;
            let y = self.ys[0];
            let f1 = self.f1s[0] + y * dt;
            return (y, f1);
        }
        if x >= self.u_max {
            let last = n - 1;
            let dt = x - self.u_max;
            let y = self.ys[last];
            let f1 = self.f1s[last] + y * dt;
            return (y, f1);
        }
        let pos = (x - self.u_min) * self.inv_dx;
        let k = (pos.floor() as usize).min(n - 2);
        let t = x - (self.u_min + k as f64 * self.dx);
        let slope = (self.ys[k + 1] - self.ys[k]) * self.inv_dx;
        let y = self.ys[k] + slope * t;
        let f1 = self.f1s[k] + self.ys[k] * t + 0.5 * slope * t * t;
        (y, f1)
    }
}

fn triode_lut() -> &'static KorenTriodeLut {
    static LUT: OnceLock<KorenTriodeLut> = OnceLock::new();
    LUT.get_or_init(KorenTriodeLut::build)
}

/// Una etapa tríodo 12AX7 con memoria de *grid current*, desplazamiento de cátodo, Miller LP y acoplo DC.
struct TriodeStage {
    prev_u: f64,
    prev_f1: f64,
    grid_env: f64,
    cath_env: f64,
    grid_att_coeff: f64,
    grid_rel_coeff: f64,
    cath_coeff: f64,
    dc_block: DcBlocker,
    miller_lp: SvfTpt,
}

impl TriodeStage {
    fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            prev_u: 0.0,
            prev_f1: 0.0,
            grid_env: 0.0,
            cath_env: 0.0,
            grid_att_coeff: 0.0,
            grid_rel_coeff: 0.0,
            cath_coeff: 0.0,
            dc_block: DcBlocker::new(18.0),
            miller_lp: SvfTpt::new(sample_rate),
        };
        s.prepare(sample_rate);
        s
    }

    fn prepare(&mut self, os_sr: f32) {
        let sr = (os_sr as f64).max(1.0);
        self.grid_att_coeff = (-1.0 / (0.0015 * sr)).exp();
        self.grid_rel_coeff = (-1.0 / (0.035 * sr)).exp();
        self.cath_coeff = (-1.0 / (0.012 * sr)).exp();
        self.dc_block.prepare(os_sr);
        self.miller_lp.set_sample_rate(os_sr);
        self.miller_lp.set_params(
            SvfMode::LowPass,
            9_500.0_f32.min(0.42 * os_sr),
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        self.reset();
    }

    fn reset(&mut self) {
        self.prev_u = 0.0;
        self.prev_f1 = 0.0;
        self.grid_env = 0.0;
        self.cath_env = 0.0;
        self.dc_block.reset();
        self.miller_lp.reset();
    }

    #[inline]
    fn process_sample(&mut self, v_in: f64, sag_frac: f64, lut: &KorenTriodeLut) -> f64 {
        // Detección de conducción de rejilla positiva y envolvente de cátodo (Rk || Ck).
        let grid_excess = (v_in - 0.14).max(0.0);
        let g_c = if grid_excess > self.grid_env {
            self.grid_att_coeff
        } else {
            self.grid_rel_coeff
        };
        self.grid_env = grid_excess + g_c * (self.grid_env - grid_excess);

        let cath_drive = v_in.abs().min(3.5);
        self.cath_env = cath_drive + self.cath_coeff * (self.cath_env - cath_drive);

        let bias_shift = sag_frac * (0.75 * self.grid_env + 0.30 * self.cath_env);
        let u_eff = (v_in - bias_shift).clamp(-6.0, 6.0);

        let (_, f1) = lut.eval(u_eff);
        let du = u_eff - self.prev_u;
        let plate_ac = if du.abs() < 1e-6 {
            lut.eval(0.5 * (u_eff + self.prev_u)).0
        } else {
            (f1 - self.prev_f1) / du
        };
        self.prev_u = u_eff;
        self.prev_f1 = f1;

        let no_dc = self.dc_block.process_sample(plate_ac as f32) as f64;
        self.miller_lp.process_sample_f64(no_dc)
    }
}

/// Preamp de válvula **12AX7** con 2 (`Crunch`) o 3 (`Lead`) etapas de tríodo en cascada.
pub struct TubePreamp {
    gain_db: f32,
    stages_idx: usize,
    bias_pct: f32,
    tone_db: f32,
    level_db: f32,
    sample_rate: f32,

    gain_smoother: OnePoleSmoother,
    bias_smoother: OnePoleSmoother,
    tone_smoother: OnePoleSmoother,
    level_smoother: OnePoleSmoother,
    mode_cf: ModeCrossfader,

    oversampler: Oversampler,
    pre_hp: SvfTpt,
    stages: [TriodeStage; 3],
    presence_eq: MatchedBiquad,
    last_tone_db: f32,
}

impl Default for TubePreamp {
    fn default() -> Self {
        Self::new()
    }
}

impl TubePreamp {
    pub fn new() -> Self {
        let mut s = Self {
            gain_db: 18.0,
            stages_idx: 0,
            bias_pct: 45.0,
            tone_db: 0.0,
            level_db: -8.0,
            sample_rate: 48_000.0,
            gain_smoother: OnePoleSmoother::new(8.0, 48_000.0, db_to_lin(18.0)),
            bias_smoother: OnePoleSmoother::new(10.0, 48_000.0, 0.45),
            tone_smoother: OnePoleSmoother::new(10.0, 48_000.0, 0.0),
            level_smoother: OnePoleSmoother::new(8.0, 48_000.0, db_to_lin(-8.0)),
            mode_cf: ModeCrossfader::new(6.0, 48_000.0, 0),
            oversampler: Oversampler::new(48_000.0, 512),
            pre_hp: SvfTpt::new(192_000.0),
            stages: [
                TriodeStage::new(192_000.0),
                TriodeStage::new(192_000.0),
                TriodeStage::new(192_000.0),
            ],
            presence_eq: MatchedBiquad::new(48_000.0),
            last_tone_db: 999.0,
        };
        // Pre-inicializamos la LUT fuera del hilo de audio.
        let _ = triode_lut();
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_gain_db(&mut self, db: f32) {
        let val = TUBE_PREAMP_PARAMS[0].clamp(db);
        self.gain_db = val;
        self.gain_smoother.set_target(db_to_lin(val));
    }

    pub fn set_stages(&mut self, val: f32) {
        let idx = TUBE_PREAMP_PARAMS[1].step_index(val);
        self.stages_idx = idx;
        self.mode_cf.set_mode(idx);
    }

    pub fn set_bias_pct(&mut self, pct: f32) {
        let val = TUBE_PREAMP_PARAMS[2].clamp(pct);
        self.bias_pct = val;
        self.bias_smoother.set_target(val * 0.01);
    }

    pub fn set_tone_db(&mut self, db: f32) {
        let val = TUBE_PREAMP_PARAMS[3].clamp(db);
        self.tone_db = val;
        self.tone_smoother.set_target(val);
    }

    pub fn set_level_db(&mut self, db: f32) {
        let val = TUBE_PREAMP_PARAMS[4].clamp(db);
        self.level_db = val;
        self.level_smoother.set_target(db_to_lin(val));
    }

    #[inline]
    fn update_presence(&mut self, db: f32) {
        if (db - self.last_tone_db).abs() <= 1e-4 {
            return;
        }
        self.last_tone_db = db;
        self.presence_eq
            .set_params(MatchedFilterKind::HighShelf, 2_200.0, 0.707, db);
    }
}

impl Effect for TubePreamp {
    fn kind(&self) -> &'static str {
        "tube_preamp"
    }

    fn name(&self) -> &'static str {
        "12AX7 Preamp"
    }

    fn accent_color(&self) -> [u8; 3] {
        [220, 68, 55]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &TUBE_PREAMP_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.gain_db,
            1 => self.stages_idx as f32,
            2 => self.bias_pct,
            3 => self.tone_db,
            4 => self.level_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_gain_db(value),
            1 => self.set_stages(value),
            2 => self.set_bias_pct(value),
            3 => self.set_tone_db(value),
            4 => self.set_level_db(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.oversampler.prepare(self.sample_rate, max_block.max(1));
        let os_sr = self.oversampler.oversampled_sample_rate();

        self.gain_smoother.set_sample_rate(os_sr);
        self.bias_smoother.set_sample_rate(os_sr);
        self.mode_cf.set_sample_rate(os_sr);

        self.pre_hp.set_sample_rate(os_sr);
        self.pre_hp.set_params(
            SvfMode::HighPass,
            75.0,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );

        for st in &mut self.stages {
            st.prepare(os_sr);
        }

        self.tone_smoother.set_sample_rate(self.sample_rate);
        self.level_smoother.set_sample_rate(self.sample_rate);
        self.presence_eq.set_sample_rate(self.sample_rate);
        self.last_tone_db = 999.0;
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = sanitize_sample(*s);
        }

        let lut = triode_lut();
        let gain_sm = &mut self.gain_smoother;
        let bias_sm = &mut self.bias_smoother;
        let mode_cf = &mut self.mode_cf;
        let pre_hp = &mut self.pre_hp;
        let stages = &mut self.stages;

        self.oversampler.process_block(buf, |os_buf, _| {
            for s in os_buf.iter_mut() {
                let gain = gain_sm.next_f64();
                let sag = bias_sm.next_f64();

                // Etapa 1: entrada acondicionada + tríodo 1 (inversión -1)
                let in1 = pre_hp.process_sample_f64(*s) * gain;
                let y1 = stages[0].process_sample(in1, sag, lut);

                // Etapa 2: divisor interetapa + tríodo 2 (inversión +1 -> fase acorde con entrada)
                let y2 = stages[1].process_sample(y1 * 1.45, sag, lut);

                // Etapa 3: tríodo 3 para 3x Lead (-1 en físico interetapa; alineamos fase en la suma de salida)
                let y3 = -stages[2].process_sample(y2 * 1.55, sag, lut) * 0.78;

                let (prev_m, next_m) = mode_cf.pair();
                let (g_prev, g_next) = mode_cf.advance_gains();
                let a = if prev_m == 0 { y2 } else { y3 };
                let b = if next_m == 0 { y2 } else { y3 };
                *s = a * (g_prev as f64) + b * (g_next as f64);
            }
        });

        for s in buf.iter_mut() {
            let t_db = self.tone_smoother.next_val();
            self.update_presence(t_db);
            let shaped = self.presence_eq.process_sample(*s);
            let lvl = self.level_smoother.next_val();
            *s = shaped * lvl;
        }
    }

    fn reset(&mut self) {
        self.oversampler.reset();
        self.pre_hp.reset();
        for st in &mut self.stages {
            st.reset();
        }
        self.presence_eq.reset();
        self.gain_smoother.reset(db_to_lin(self.gain_db));
        self.bias_smoother.reset(self.bias_pct * 0.01);
        self.tone_smoother.reset(self.tone_db);
        self.level_smoother.reset(db_to_lin(self.level_db));
        self.mode_cf.reset(self.stages_idx);
        self.last_tone_db = 999.0;
        self.update_presence(self.tone_db);
    }
}
