//! Noise Gate avanzado (`D9`):
//! - 5 perillas: `Threshold`, `Hyst` (histéresis de doble umbral), `Hold`, `Release` y `Range`.
//! - *Sidechain* interno fijo (`SvfTpt` pasa-altos de 4.º orden a `120 Hz` + pasa-bajos a `4.5 kHz`)
//!   sin alterar el timbre de la señal principal.
//! - *Lookahead* fijo de `1 ms` (línea de retardo prealojada en `prepare()`) para abrir antes del ataque.

use crate::svf::{SvfMode, SvfTpt};
use crate::{db_to_lin, lin_to_db, sanitize_sample, Effect, ParamScale, ParamSpec};

const GATE_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "threshold_db",
        name: "Threshold",
        unit: "dB",
        min: -100.0,
        max: 0.0,
        default: -60.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "hysteresis_db",
        name: "Hyst",
        unit: "dB",
        min: 0.0,
        max: 12.0,
        default: 6.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "hold_ms",
        name: "Hold",
        unit: "ms",
        min: 0.0,
        max: 500.0,
        default: 50.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "release_ms",
        name: "Release",
        unit: "ms",
        min: 5.0,
        max: 1_000.0,
        default: 100.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "range_db",
        name: "Range",
        unit: "dB",
        min: -80.0,
        max: 0.0,
        default: -80.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
];

/// Puerta de ruido avanzada con histéresis, filtro *sidechain*, piso `Range` y *lookahead* de `1 ms`.
pub struct NoiseGate {
    threshold_db: f32,
    hysteresis_db: f32,
    hold_ms: f32,
    release_ms: f32,
    range_db: f32,
    sample_rate: f32,

    sc_hp1: SvfTpt,
    sc_hp2: SvfTpt,
    sc_lp: SvfTpt,

    env_attack_coeff: f32,
    env_release_coeff: f32,
    gain_open_coeff: f32,
    gain_close_coeff: f32,
    hold_samples: usize,
    hold_counter: usize,

    envelope: f32,
    open: bool,
    gain: f32,
    range_lin: f32,

    delay_buf: Vec<f32>,
    delay_pos: usize,
}

impl NoiseGate {
    pub fn new(threshold_db: f32) -> Self {
        let mut s = Self {
            threshold_db: -60.0,
            hysteresis_db: 6.0,
            hold_ms: 50.0,
            release_ms: 100.0,
            range_db: -80.0,
            sample_rate: 48_000.0,
            sc_hp1: SvfTpt::new(48_000.0),
            sc_hp2: SvfTpt::new(48_000.0),
            sc_lp: SvfTpt::new(48_000.0),
            env_attack_coeff: 0.0,
            env_release_coeff: 0.0,
            gain_open_coeff: 0.0,
            gain_close_coeff: 0.0,
            hold_samples: 0,
            hold_counter: 0,
            envelope: 0.0,
            open: false,
            gain: 0.0,
            range_lin: 0.0,
            delay_buf: Vec::new(),
            delay_pos: 0,
        };
        s.set_threshold_db(threshold_db);
        s.prepare(48_000.0, 512);
        s
    }

    /// Ajusta el umbral de apertura en dBFS (`<= -100 dB` deja la puerta siempre abierta).
    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        let db = if threshold_db.is_finite() {
            threshold_db.clamp(-120.0, 0.0)
        } else {
            -120.0
        };
        self.threshold_db = db;
    }

    pub fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    pub fn set_hysteresis_db(&mut self, db: f32) {
        self.hysteresis_db = GATE_PARAMS[1].clamp(db);
    }

    pub fn set_hold_ms(&mut self, ms: f32) {
        self.hold_ms = GATE_PARAMS[2].clamp(ms);
        self.hold_samples = ((self.hold_ms * 1e-3) * self.sample_rate).round() as usize;
    }

    pub fn set_release_ms(&mut self, ms: f32) {
        self.release_ms = GATE_PARAMS[3].clamp(ms);
        self.gain_close_coeff = (-1.0 / (self.release_ms * 1e-3 * self.sample_rate)).exp();
    }

    pub fn set_range_db(&mut self, db: f32) {
        self.range_db = GATE_PARAMS[4].clamp(db);
        self.range_lin = if self.range_db <= -79.95 {
            0.0
        } else {
            db_to_lin(self.range_db)
        };
    }

    /// Latencia fija en muestras introducida por el *lookahead* (`1 ms`).
    #[inline]
    pub fn latency_samples(&self) -> usize {
        self.delay_buf.len()
    }

    /// Estado actual de la máquina de histéresis (`true` = puerta abierta).
    #[inline]
    pub fn is_open(&self) -> bool {
        self.open
    }
}

impl Effect for NoiseGate {
    fn kind(&self) -> &'static str {
        "noise_gate"
    }

    fn name(&self) -> &'static str {
        "Noise Gate"
    }

    fn accent_color(&self) -> [u8; 3] {
        [56, 189, 248]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &GATE_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.threshold_db,
            1 => self.hysteresis_db,
            2 => self.hold_ms,
            3 => self.release_ms,
            4 => self.range_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_threshold_db(value),
            1 => self.set_hysteresis_db(value),
            2 => self.set_hold_ms(value),
            3 => self.set_release_ms(value),
            4 => self.set_range_db(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;

        // Sidechain interno: HP 4.º orden (120 Hz) + LP 2.º orden (4.5 kHz).
        let q = std::f32::consts::FRAC_1_SQRT_2;
        self.sc_hp1.set_sample_rate(sr);
        self.sc_hp1.set_params(SvfMode::HighPass, 120.0, q, 0.0);
        self.sc_hp2.set_sample_rate(sr);
        self.sc_hp2.set_params(SvfMode::HighPass, 120.0, q, 0.0);
        self.sc_lp.set_sample_rate(sr);
        self.sc_lp
            .set_params(SvfMode::LowPass, 4_500.0_f32.min(0.45 * sr), q, 0.0);

        self.env_attack_coeff = (-1.0 / (0.00025 * sr)).exp();
        self.env_release_coeff = (-1.0 / (0.030 * sr)).exp();
        self.gain_open_coeff = (-1.0 / (0.0005 * sr)).exp();
        self.gain_close_coeff = (-1.0 / (self.release_ms * 1e-3 * sr)).exp();
        self.hold_samples = ((self.hold_ms * 1e-3) * sr).round() as usize;

        let lookahead_len = (0.001 * sr).round().max(1.0) as usize;
        self.delay_buf.resize(lookahead_len, 0.0);
        self.delay_buf.fill(0.0);
        self.delay_pos = 0;

        self.range_lin = if self.range_db <= -79.95 {
            0.0
        } else {
            db_to_lin(self.range_db)
        };
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        let delay_len = self.delay_buf.len();
        if self.threshold_db <= -100.0 {
            self.open = true;
            self.gain = 1.0;
            for s in buf.iter_mut() {
                let x = sanitize_sample(*s);
                let delayed = self.delay_buf[self.delay_pos];
                self.delay_buf[self.delay_pos] = x;
                self.delay_pos = (self.delay_pos + 1) % delay_len;
                *s = delayed;
            }
            return;
        }

        let close_thresh = self.threshold_db - self.hysteresis_db;

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s);

            // Cadena lateral filtrada (nunca se mezcla en la salida de audio).
            let hp1 = self.sc_hp1.process_sample(x);
            let hp2 = self.sc_hp2.process_sample(hp1);
            let sc = self.sc_lp.process_sample(hp2);
            let level = sc.abs();

            if level > self.envelope {
                self.envelope =
                    self.env_attack_coeff * self.envelope + (1.0 - self.env_attack_coeff) * level;
            } else {
                self.envelope =
                    self.env_release_coeff * self.envelope + (1.0 - self.env_release_coeff) * level;
            }
            if self.envelope < 1e-12 {
                self.envelope = 0.0;
            }

            let env_db = lin_to_db(self.envelope);

            if !self.open {
                if env_db >= self.threshold_db {
                    self.open = true;
                    self.hold_counter = self.hold_samples;
                }
            } else if env_db >= close_thresh {
                self.hold_counter = self.hold_samples;
            } else if self.hold_counter > 0 {
                self.hold_counter -= 1;
            } else {
                self.open = false;
            }

            let target_gain = if self.open { 1.0 } else { self.range_lin };
            let coeff = if target_gain > self.gain {
                self.gain_open_coeff
            } else {
                self.gain_close_coeff
            };
            self.gain = coeff * self.gain + (1.0 - coeff) * target_gain;
            if target_gain == 0.0 && self.gain < 1e-7 {
                self.gain = 0.0;
            }

            let delayed = self.delay_buf[self.delay_pos];
            self.delay_buf[self.delay_pos] = x;
            self.delay_pos = (self.delay_pos + 1) % delay_len;
            *s = delayed * self.gain;
        }
    }

    fn reset(&mut self) {
        self.sc_hp1.reset();
        self.sc_hp2.reset();
        self.sc_lp.reset();
        self.hold_counter = 0;
        self.envelope = 0.0;
        self.delay_buf.fill(0.0);
        self.delay_pos = 0;
        if self.threshold_db <= -100.0 {
            self.open = true;
            self.gain = 1.0;
        } else {
            self.open = false;
            self.gain = self.range_lin;
        }
    }
}
