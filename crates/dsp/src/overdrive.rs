use crate::{db_to_lin, sanitize_sample, DcBlocker, Effect};

/// Pedal de overdrive asimétrico basado en saturación suave `tanh`.
///
/// Flujo: bloqueador DC -> pre-ganancia (`drive` dB) -> recorte asimétrico ->
/// bloqueador DC -> filtro pasa-bajos de tono (1 polo) -> nivel de salida (`level` dB).
#[derive(Debug, Clone)]
pub struct Overdrive {
    drive_db: f32,
    pre_gain: f32,
    tone_hz: f32,
    tone_alpha: f32,
    level_db: f32,
    level_lin: f32,
    sample_rate: f32,
    pre_dc: DcBlocker,
    post_dc: DcBlocker,
    lp_state: f32,
}

impl Default for Overdrive {
    fn default() -> Self {
        Self::new()
    }
}

impl Overdrive {
    pub fn new() -> Self {
        let mut s = Self {
            drive_db: 12.0,
            pre_gain: db_to_lin(12.0),
            tone_hz: 4_000.0,
            tone_alpha: 0.4,
            level_db: -6.0,
            level_lin: db_to_lin(-6.0),
            sample_rate: 48_000.0,
            pre_dc: DcBlocker::new(15.0),
            post_dc: DcBlocker::new(15.0),
            lp_state: 0.0,
        };
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_drive_db(&mut self, drive_db: f32) {
        let db = if drive_db.is_finite() {
            drive_db.clamp(0.0, 40.0)
        } else {
            0.0
        };
        if (db - self.drive_db).abs() <= f32::EPSILON {
            return;
        }
        self.drive_db = db;
        self.pre_gain = db_to_lin(db);
    }

    pub fn set_tone_hz(&mut self, tone_hz: f32) {
        let hz = if tone_hz.is_finite() {
            tone_hz.clamp(500.0, 12_000.0)
        } else {
            4_000.0
        };
        if (hz - self.tone_hz).abs() <= f32::EPSILON {
            return;
        }
        self.tone_hz = hz;
        self.recompute_tone_alpha();
    }

    pub fn set_level_db(&mut self, level_db: f32) {
        let db = if level_db.is_finite() {
            level_db.clamp(-120.0, 12.0)
        } else {
            -120.0
        };
        if (db - self.level_db).abs() <= f32::EPSILON {
            return;
        }
        self.level_db = db;
        self.level_lin = db_to_lin(db);
    }

    fn recompute_tone_alpha(&mut self) {
        let dt = 1.0 / self.sample_rate.max(1.0);
        let rc = 1.0 / (2.0 * std::f32::consts::PI * self.tone_hz);
        self.tone_alpha = (dt / (rc + dt)).clamp(0.001, 1.0);
    }
}

impl Effect for Overdrive {
    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.pre_dc.prepare(self.sample_rate);
        self.post_dc.prepare(self.sample_rate);
        self.recompute_tone_alpha();
        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        const BIAS: f32 = 0.15;
        let bias_tanh = BIAS.tanh();
        // Factor 1.15 deja margen frente al transitorio inicial del bloqueador DC para no exceder ±1.0.
        let norm = (1.0 + bias_tanh) * 1.15;

        for s in buf.iter_mut() {
            let clean = self.pre_dc.process_sample(sanitize_sample(*s));
            let pre = (clean * self.pre_gain).clamp(-100.0, 100.0);

            // TODO: Agregar oversampling (ej. 2x o 4x) en trabajo futuro para reducir aliasing con drive alto.
            let shaped = ((pre + BIAS).tanh() - bias_tanh) / norm;

            let no_dc = self.post_dc.process_sample(shaped);
            self.lp_state += self.tone_alpha * (no_dc - self.lp_state);
            if !self.lp_state.is_finite() {
                self.lp_state = 0.0;
            }
            *s = self.lp_state * self.level_lin;
        }
    }

    fn reset(&mut self) {
        self.pre_dc.reset();
        self.post_dc.reset();
        self.lp_state = 0.0;
    }
}
