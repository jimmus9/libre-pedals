//! Filtro de variable de estado en topología preservada (*Cytomic SVF-TPT*, Simper 2013).
//!
//! A diferencia de un *biquad* Direct-Form, el integrador trapezoidal ZDF mantiene el acoplamiento
//! ortogonal de frecuencia de corte y resonancia bajo modulación rápida por muestra (útil en
//! Wah/Auto-Wah, *sidechain* dinámico y filtros tono de overdrive) sin explotar ni perder estabilidad.

use std::f64::consts::PI;

use crate::denormals::flush_denormal_f64;

/// Respuesta seleccionada en [`SvfTpt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvfMode {
    LowPass,
    HighPass,
    BandPass,
    Notch,
    AllPass,
    Peak,
    Bell,
    LowShelf,
    HighShelf,
}

/// Filtro SVF-TPT de 2.º orden con estado interno y coeficientes en doble precisión (`f64`).
#[derive(Debug, Clone, Copy)]
pub struct SvfTpt {
    sample_rate: f64,
    mode: SvfMode,
    cutoff_hz: f64,
    q: f64,
    gain_db: f64,
    g: f64,
    k: f64,
    a1: f64,
    a2: f64,
    a3: f64,
    m0: f64,
    m1: f64,
    m2: f64,
    ic1eq: f64,
    ic2eq: f64,
}

impl SvfTpt {
    /// Crea un SVF-TPT en modo pasa-bajos a 1 kHz, Q = 0.707.
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            sample_rate: (sample_rate as f64).max(1.0),
            mode: SvfMode::LowPass,
            cutoff_hz: 1000.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
            gain_db: 0.0,
            g: 0.0,
            k: 0.0,
            a1: 0.0,
            a2: 0.0,
            a3: 0.0,
            m0: 0.0,
            m1: 0.0,
            m2: 1.0,
            ic1eq: 0.0,
            ic2eq: 0.0,
        };
        s.update_coefficients();
        s
    }

    /// Actualiza la frecuencia de muestreo y recalcula los coeficientes manteniendo el estado.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = (sample_rate as f64).max(1.0);
        self.update_coefficients();
    }

    /// Borra el estado interno de los dos integradores.
    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }

    /// Configura modo, frecuencia de corte (`Hz`), factor `Q` y ganancia (`dB`, para `Bell/Shelf`).
    pub fn set_params(&mut self, mode: SvfMode, cutoff_hz: f32, q: f32, gain_db: f32) {
        self.mode = mode;
        let max_fc = 0.49 * self.sample_rate;
        self.cutoff_hz = (cutoff_hz as f64).clamp(5.0, max_fc.max(10.0));
        self.q = (q as f64).clamp(0.05, 40.0);
        self.gain_db = if gain_db.is_finite() {
            (gain_db as f64).clamp(-48.0, 48.0)
        } else {
            0.0
        };
        self.update_coefficients();
    }

    fn update_coefficients(&mut self) {
        let tan_w = (PI * self.cutoff_hz / self.sample_rate).tan();
        let a = 10.0_f64.powf(self.gain_db / 40.0);

        match self.mode {
            SvfMode::LowPass => {
                self.g = tan_w;
                self.k = 1.0 / self.q;
                self.m0 = 0.0;
                self.m1 = 0.0;
                self.m2 = 1.0;
            }
            SvfMode::HighPass => {
                self.g = tan_w;
                self.k = 1.0 / self.q;
                self.m0 = 1.0;
                self.m1 = -self.k;
                self.m2 = -1.0;
            }
            SvfMode::BandPass => {
                // Pasa-banda de ganancia pico unitaria (0 dB en fc) con ancho de banda fijado por Q.
                self.g = tan_w;
                self.k = 1.0 / self.q;
                self.m0 = 0.0;
                self.m1 = self.k;
                self.m2 = 0.0;
            }
            SvfMode::Notch => {
                self.g = tan_w;
                self.k = 1.0 / self.q;
                self.m0 = 1.0;
                self.m1 = -self.k;
                self.m2 = 0.0;
            }
            SvfMode::AllPass => {
                self.g = tan_w;
                self.k = 1.0 / self.q;
                self.m0 = 1.0;
                self.m1 = -2.0 * self.k;
                self.m2 = 0.0;
            }
            SvfMode::Peak => {
                self.g = tan_w;
                self.k = 1.0 / self.q;
                self.m0 = 1.0;
                self.m1 = -self.k;
                self.m2 = -2.0;
            }
            SvfMode::Bell => {
                self.g = tan_w;
                self.k = 1.0 / (self.q * a);
                self.m0 = 1.0;
                self.m1 = self.k * (a * a - 1.0);
                self.m2 = 0.0;
            }
            SvfMode::LowShelf => {
                self.g = tan_w / a.sqrt();
                self.k = 1.0 / self.q;
                self.m0 = 1.0;
                self.m1 = self.k * (a - 1.0);
                self.m2 = a * a - 1.0;
            }
            SvfMode::HighShelf => {
                self.g = tan_w * a.sqrt();
                self.k = 1.0 / self.q;
                self.m0 = a * a;
                self.m1 = self.k * (1.0 - a) * a;
                self.m2 = 1.0 - a * a;
            }
        }

        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    /// Procesa una muestra en doble precisión (`f64`).
    #[inline]
    pub fn process_sample_f64(&mut self, v0: f64) -> f64 {
        let v3 = v0 - self.ic2eq;
        let v1 = self.a1 * self.ic1eq + self.a2 * v3;
        let v2 = self.ic2eq + self.a2 * self.ic1eq + self.a3 * v3;
        self.ic1eq = flush_denormal_f64(2.0 * v1 - self.ic1eq);
        self.ic2eq = flush_denormal_f64(2.0 * v2 - self.ic2eq);
        self.m0 * v0 + self.m1 * v1 + self.m2 * v2
    }

    /// Procesa una muestra en precisión simple (`f32`) manteniendo el estado interno en `f64`.
    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let x = if input.is_finite() { input as f64 } else { 0.0 };
        self.process_sample_f64(x) as f32
    }

    /// Calcula la magnitud lineal exacta de la respuesta en frecuencia a `freq_hz`.
    pub fn magnitude_response(&self, freq_hz: f32) -> f32 {
        let f = (freq_hz as f64).clamp(0.0, 0.4999 * self.sample_rate);
        let w = 2.0 * PI * f / self.sample_rate;
        let sd = (0.5 * w).tan() / self.g.max(1e-12);
        let s2 = -sd * sd;
        let num_re = self.m0 * s2 + (self.m0 + self.m2);
        let num_im = (self.m0 * self.k + self.m1) * sd;
        let den_re = s2 + 1.0;
        let den_im = self.k * sd;
        let num_mag = (num_re * num_re + num_im * num_im).sqrt();
        let den_mag = (den_re * den_re + den_im * den_im).sqrt().max(1e-18);
        (num_mag / den_mag) as f32
    }

    /// Magnitud en decibelios a `freq_hz`.
    #[inline]
    pub fn magnitude_db_at(&self, freq_hz: f32) -> f32 {
        crate::lin_to_db(self.magnitude_response(freq_hz))
    }
}
