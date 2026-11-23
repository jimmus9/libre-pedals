use crate::{db_to_lin, sanitize_sample, Effect};

/// Pedal de ganancia / volumen con suavizado lineal por muestra para evitar clics.
#[derive(Debug, Clone)]
pub struct Gain {
    db: f32,
    current_lin: f32,
    target_lin: f32,
    step: f32,
    ramp_samples: f32,
}

impl Gain {
    pub fn new(db: f32) -> Self {
        let db = if db.is_finite() { db.clamp(-120.0, 24.0) } else { 0.0 };
        let lin = db_to_lin(db);
        let ramp_samples = 480.0; // 10 ms @ 48 kHz
        Self {
            db,
            current_lin: lin,
            target_lin: lin,
            step: 0.0,
            ramp_samples,
        }
    }

    pub fn set_db(&mut self, db: f32) {
        let db = if db.is_finite() { db.clamp(-120.0, 24.0) } else { -120.0 };
        let new_lin = db_to_lin(db);
        if (new_lin - self.target_lin).abs() <= f32::EPSILON {
            return;
        }
        self.db = db;
        self.target_lin = new_lin;
        self.step = ((self.target_lin - self.current_lin).abs() / self.ramp_samples.max(1.0))
            .max(1e-7);
    }

    pub fn db(&self) -> f32 {
        self.db
    }
}

impl Effect for Gain {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.ramp_samples = (0.010 * sr).max(1.0);
        if (self.target_lin - self.current_lin).abs() > f32::EPSILON {
            self.step = ((self.target_lin - self.current_lin).abs() / self.ramp_samples)
                .max(1e-7);
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        if (self.current_lin - self.target_lin).abs() <= f32::EPSILON {
            self.current_lin = self.target_lin;
            if (self.target_lin - 1.0).abs() <= f32::EPSILON {
                for s in buf.iter_mut() {
                    if !s.is_finite() {
                        *s = 0.0;
                    }
                }
                return;
            }
            for s in buf.iter_mut() {
                *s = sanitize_sample(*s) * self.target_lin;
            }
            return;
        }

        for s in buf.iter_mut() {
            let diff = self.target_lin - self.current_lin;
            if diff.abs() <= self.step {
                self.current_lin = self.target_lin;
            } else {
                self.current_lin += self.step * diff.signum();
            }
            *s = sanitize_sample(*s) * self.current_lin;
        }
    }

    fn reset(&mut self) {
        self.current_lin = self.target_lin;
        self.step = 0.0;
    }
}
