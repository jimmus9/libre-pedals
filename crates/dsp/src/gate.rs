use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const GATE_PARAMS: [ParamSpec; 1] = [ParamSpec {
    name: "Threshold",
    unit: "dB",
    min: -100.0,
    max: 0.0,
    default: -60.0,
    scale: ParamScale::Linear,
    decimals: 1,
}];

/// Puerta de ruido con seguidor de envolvente de pico, hold y suavizado de ganancia.
#[derive(Debug, Clone)]
pub struct NoiseGate {
    threshold_db: f32,
    threshold_lin: f32,
    sample_rate: f32,
    attack_coeff: f32,
    release_coeff: f32,
    gain_open_coeff: f32,
    gain_close_coeff: f32,
    hold_samples: usize,
    hold_counter: usize,
    envelope: f32,
    gain: f32,
}

impl NoiseGate {
    pub fn new(threshold_db: f32) -> Self {
        let mut s = Self {
            threshold_db: -60.0,
            threshold_lin: 0.0,
            sample_rate: 48_000.0,
            attack_coeff: 0.0,
            release_coeff: 0.0,
            gain_open_coeff: 0.0,
            gain_close_coeff: 0.0,
            hold_samples: 0,
            hold_counter: 0,
            envelope: 0.0,
            gain: 0.0,
        };
        s.set_threshold_db(threshold_db);
        s.prepare(48_000.0, 512);
        s
    }

    /// Ajusta el umbral en dBFS. Valores <= -100 dB desactivan la puerta (siempre abierta).
    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        let db = if threshold_db.is_finite() {
            threshold_db.clamp(-120.0, 0.0)
        } else {
            -120.0
        };
        self.threshold_db = db;
        self.threshold_lin = if db <= -100.0 { 0.0 } else { db_to_lin(db) };
    }

    pub fn threshold_db(&self) -> f32 {
        self.threshold_db
    }
}

impl Effect for NoiseGate {
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
            0 => self.threshold_db(),
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index == 0 {
            self.set_threshold_db(value);
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        // Attack ~1 ms, release ~100 ms, hold ~50 ms.
        self.attack_coeff = (-1.0 / (0.001 * sr)).exp();
        self.release_coeff = (-1.0 / (0.100 * sr)).exp();
        // Suavizado de ganancia para evitar clics (apertura ~1 ms, cierre ~50 ms).
        self.gain_open_coeff = (-1.0 / (0.001 * sr)).exp();
        self.gain_close_coeff = (-1.0 / (0.050 * sr)).exp();
        self.hold_samples = (0.050 * sr).round() as usize;
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        if self.threshold_db <= -100.0 {
            self.gain = 1.0;
            for s in buf.iter_mut() {
                *s = sanitize_sample(*s);
            }
            return;
        }

        for s in buf.iter_mut() {
            let x = sanitize_sample(*s);
            let level = x.abs();

            if level > self.envelope {
                self.envelope = self.attack_coeff * self.envelope + (1.0 - self.attack_coeff) * level;
            } else {
                self.envelope = self.release_coeff * self.envelope + (1.0 - self.release_coeff) * level;
            }
            if self.envelope < 1e-12 {
                self.envelope = 0.0;
            }

            let target_gain = if self.envelope >= self.threshold_lin {
                self.hold_counter = self.hold_samples;
                1.0
            } else if self.hold_counter > 0 {
                self.hold_counter -= 1;
                1.0
            } else {
                0.0
            };

            let coeff = if target_gain > self.gain {
                self.gain_open_coeff
            } else {
                self.gain_close_coeff
            };
            self.gain = coeff * self.gain + (1.0 - coeff) * target_gain;
            if self.gain < 1e-7 && target_gain == 0.0 {
                self.gain = 0.0;
            }

            *s = x * self.gain;
        }
    }

    fn reset(&mut self) {
        self.hold_counter = 0;
        self.envelope = 0.0;
        self.gain = if self.threshold_db <= -100.0 { 1.0 } else { 0.0 };
    }
}
