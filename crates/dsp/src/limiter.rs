//! Limitador *Brickwall* con *Lookahead* (`1.5 ms`) y detección *True-Peak* (`ITU-R BS.1770`) (`D11`):
//! - Factor *True-Peak* adaptativo continuo: `4x` si `fs <= 50 kHz`, `2x` si `50 < fs < 176.4 kHz`,
//!   `1x` si `fs >= 176.4 kHz`.
//! - Mínimo móvil en $\mathcal{O}(1)$ por muestra mediante cola monótona en anillo circular + media
//!   móvil box de igual longitud `L+1`, garantizando `0.00 dB` de *overshoot* sin recortes duros.

use std::f64::consts::PI;

use crate::smoother::OnePoleSmoother;
use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const FIR_TAPS: usize = 16;

const LIMITER_PARAMS: [ParamSpec; 3] = [
    ParamSpec {
        id: "drive_db",
        name: "Drive",
        unit: "dB",
        min: 0.0,
        max: 24.0,
        default: 0.0,
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
        id: "release_ms",
        name: "Release",
        unit: "ms",
        min: 10.0,
        max: 1_000.0,
        default: 120.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
];

/// Limitador Brickwall transparente con *lookahead* de `1.5 ms` y detector *True-Peak* polifásico.
pub struct BrickwallLimiter {
    drive_db: f32,
    ceiling_db: f32,
    release_ms: f32,
    sample_rate: f64,

    drive_smoother: OnePoleSmoother,
    ceiling_lin: f64,
    release_alpha: f64,

    tp_factor: usize,
    det_delay: usize,
    lookahead_len: usize,
    fir_phases: Vec<[f64; FIR_TAPS]>,
    fir_hist: [f64; FIR_TAPS],
    fir_pos: usize,

    // Cola monótona decreciente (para mínimo móvil en ventana L+1) sobre anillo circular preasignado.
    deque_idx: Vec<u64>,
    deque_val: Vec<f64>,
    deque_head: usize,
    deque_len: usize,
    sample_counter: u64,

    // Media móvil box de longitud L+1 sobre la salida del mínimo móvil.
    box_buf: Vec<f64>,
    box_pos: usize,
    box_sum: f64,

    g_out_prev: f64,
    audio_delay: Vec<f64>,
    audio_pos: usize,
}

impl Default for BrickwallLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl BrickwallLimiter {
    pub fn new() -> Self {
        let mut s = Self {
            drive_db: 0.0,
            ceiling_db: -1.0,
            release_ms: 120.0,
            sample_rate: 48_000.0,
            drive_smoother: OnePoleSmoother::new(10.0, 48_000.0, db_to_lin(0.0)),
            ceiling_lin: db_to_lin(-1.0) as f64,
            release_alpha: 0.0,
            tp_factor: 4,
            det_delay: 8,
            lookahead_len: 72,
            fir_phases: Vec::new(),
            fir_hist: [0.0; FIR_TAPS],
            fir_pos: 0,
            deque_idx: Vec::new(),
            deque_val: Vec::new(),
            deque_head: 0,
            deque_len: 0,
            sample_counter: 0,
            box_buf: Vec::new(),
            box_pos: 0,
            box_sum: 0.0,
            g_out_prev: 1.0,
            audio_delay: Vec::new(),
            audio_pos: 0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_drive_db(&mut self, db: f32) {
        let val = LIMITER_PARAMS[0].clamp(db);
        self.drive_db = val;
        self.drive_smoother.set_target(db_to_lin(val));
    }

    pub fn set_ceiling_db(&mut self, db: f32) {
        let val = LIMITER_PARAMS[1].clamp(db);
        self.ceiling_db = val;
        self.ceiling_lin = 10.0_f64.powf(val as f64 * 0.05);
    }

    pub fn set_release_ms(&mut self, ms: f32) {
        let val = LIMITER_PARAMS[2].clamp(ms);
        self.release_ms = val;
        self.release_alpha = 1.0 - (-1.0 / (val as f64 * 1e-3 * self.sample_rate)).exp();
    }

    /// Latencia total en muestras (`L_lookahead + T_true_peak`).
    #[inline]
    pub fn latency_samples(&self) -> usize {
        self.audio_delay.len()
    }

    fn build_polyphase_fir(factor: usize) -> Vec<[f64; FIR_TAPS]> {
        if factor <= 1 {
            return Vec::new();
        }
        let mut phases = Vec::with_capacity(factor - 1);
        for p in 1..factor {
            let frac = p as f64 / factor as f64;
            let mut coefs = [0.0_f64; FIR_TAPS];
            let mut sum = 0.0_f64;
            for (k, c) in coefs.iter_mut().enumerate() {
                let t = frac - (k as f64 - 7.0);
                let sinc = if t.abs() < 1e-12 {
                    1.0
                } else {
                    (PI * t).sin() / (PI * t)
                };
                let win = 0.5 * (1.0 + (PI * t / 8.5).cos());
                *c = sinc * win;
                sum += *c;
            }
            if sum.abs() > 1e-12 {
                for c in &mut coefs {
                    *c /= sum;
                }
            }
            phases.push(coefs);
        }
        phases
    }
}

impl Effect for BrickwallLimiter {
    fn kind(&self) -> &'static str {
        "limiter"
    }

    fn name(&self) -> &'static str {
        "Brickwall Limiter"
    }

    fn accent_color(&self) -> [u8; 3] {
        [234, 179, 8]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &LIMITER_PARAMS
    }

    fn latency_samples(&self) -> usize {
        self.audio_delay.len()
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.drive_db,
            1 => self.ceiling_db,
            2 => self.release_ms,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_drive_db(value),
            1 => self.set_ceiling_db(value),
            2 => self.set_release_ms(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = (sample_rate as f64).max(1.0);
        self.sample_rate = sr;
        self.drive_smoother.set_sample_rate(sr as f32);
        self.release_alpha = 1.0 - (-1.0 / (self.release_ms as f64 * 1e-3 * sr)).exp();

        let tp_factor = if sr <= 50_000.0 {
            4
        } else if sr < 176_400.0 {
            2
        } else {
            1
        };
        let det_delay = if tp_factor > 1 { 8 } else { 0 };
        let lookahead_len = (0.0015 * sr).round().max(1.0) as usize;
        let box_len = lookahead_len + 1;
        let deque_cap = box_len + 1;

        self.tp_factor = tp_factor;
        self.det_delay = det_delay;
        self.lookahead_len = lookahead_len;
        self.fir_phases = Self::build_polyphase_fir(tp_factor);

        self.deque_idx.resize(deque_cap, 0);
        self.deque_val.resize(deque_cap, 1.0);
        self.box_buf.resize(box_len, 1.0);
        self.audio_delay.resize(lookahead_len + det_delay, 0.0);

        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let box_len = self.box_buf.len();
        let deque_cap = self.deque_val.len();
        let audio_len = self.audio_delay.len();
        let win_span = (self.lookahead_len + 1) as u64;
        let c_lim = self.ceiling_lin;

        for s in buf.iter_mut() {
            let drive = self.drive_smoother.next_f64();
            let x = (sanitize_sample(*s) as f64) * drive;

            // 1. Detección True-Peak: si M > 1, evaluamos j = n - 8 con fases inter-muestra.
            let tp = if self.tp_factor > 1 {
                self.fir_hist[self.fir_pos] = x;
                // Base más antigua = fir_pos + 1 (muestra n - 15); k = 7 es muestra n - 8.
                let oldest = (self.fir_pos + 1) & (FIR_TAPS - 1);
                let center_idx = (oldest + 7) & (FIR_TAPS - 1);
                let mut peak = self.fir_hist[center_idx].abs();

                for phase in &self.fir_phases {
                    let mut acc = 0.0_f64;
                    for (k, &h) in phase.iter().enumerate() {
                        let idx = (oldest + k) & (FIR_TAPS - 1);
                        acc += h * self.fir_hist[idx];
                    }
                    let abs_acc = acc.abs();
                    if abs_acc > peak {
                        peak = abs_acc;
                    }
                }
                self.fir_pos = oldest;
                peak
            } else {
                x.abs()
            };

            let g_raw = (c_lim / tp.max(1.0e-12)).min(1.0);

            // 2. Mínimo móvil O(1) en ventana [cur - L, cur] con cola monótona creciente.
            let cur = self.sample_counter;
            self.sample_counter = cur + 1;

            while self.deque_len > 0 {
                let back = (self.deque_head + self.deque_len - 1) % deque_cap;
                if self.deque_val[back] >= g_raw {
                    self.deque_len -= 1;
                } else {
                    break;
                }
            }
            let push_pos = (self.deque_head + self.deque_len) % deque_cap;
            self.deque_idx[push_pos] = cur;
            self.deque_val[push_pos] = g_raw;
            self.deque_len += 1;

            while self.deque_len > 0 {
                let f_idx = self.deque_idx[self.deque_head];
                if f_idx + win_span <= cur {
                    self.deque_head = (self.deque_head + 1) % deque_cap;
                    self.deque_len -= 1;
                } else {
                    break;
                }
            }

            let min_val = self.deque_val[self.deque_head];

            // 3. Media móvil box de longitud L+1 (garantiza g_smooth <= g_raw[cur - L]).
            let prev_box = self.box_buf[self.box_pos];
            self.box_buf[self.box_pos] = min_val;
            self.box_pos = (self.box_pos + 1) % box_len;
            self.box_sum += min_val - prev_box;
            let g_smooth = (self.box_sum / box_len as f64).clamp(0.0, 1.0);

            // 4. Ataque instantáneo sobre la curva suavizada + liberación exponencial configurable.
            let g_out = if g_smooth < self.g_out_prev {
                g_smooth
            } else {
                self.g_out_prev + (g_smooth - self.g_out_prev) * self.release_alpha
            };
            self.g_out_prev = g_out;

            // 5. Aplicar sobre la muestra de audio exactamente alineada en D = L + T.
            let delayed = self.audio_delay[self.audio_pos];
            self.audio_delay[self.audio_pos] = x;
            self.audio_pos = (self.audio_pos + 1) % audio_len;

            *s = (delayed * g_out) as f32;
        }
    }

    fn reset(&mut self) {
        self.drive_smoother.reset(db_to_lin(self.drive_db));
        self.fir_hist.fill(0.0);
        self.fir_pos = 0;
        self.deque_idx.fill(0);
        self.deque_val.fill(1.0);
        self.deque_head = 0;
        self.deque_len = 0;
        self.sample_counter = 0;
        self.box_buf.fill(1.0);
        self.box_pos = 0;
        self.box_sum = self.box_buf.len() as f64;
        self.g_out_prev = 1.0;
        self.audio_delay.fill(0.0);
        self.audio_pos = 0;
    }
}
