//! Filtros *biquad* con respuesta en magnitud emparejada (*Matched Filters*, Vicanek 2016 / Orfanidis 1997).
//!
//! Eliminan el aplastamiento de ancho de banda cerca de Nyquist (*Nyquist cramping*) propio de la
//! transformada bilineal tradicional en ecualizadores paramétricos y filtros de tono de audio a 44.1–48 kHz.

use std::f64::consts::PI;

use crate::denormals::flush_denormal_f64;

/// Tipo de filtro implementado por [`MatchedBiquad`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchedFilterKind {
    Peaking,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    BandPass,
}

/// Seccion biquad *matched* de 2.º orden (Direct-Form II Transposed en `f64`).
#[derive(Debug, Clone, Copy)]
pub struct MatchedBiquad {
    sample_rate: f64,
    kind: MatchedFilterKind,
    freq_hz: f64,
    q: f64,
    gain_db: f64,
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl MatchedBiquad {
    /// Crea un filtro *matched* inicializado plano a 1 kHz.
    pub fn new(sample_rate: f32) -> Self {
        let mut f = Self {
            sample_rate: (sample_rate as f64).max(1.0),
            kind: MatchedFilterKind::Peaking,
            freq_hz: 1000.0,
            q: std::f64::consts::FRAC_1_SQRT_2,
            gain_db: 0.0,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        };
        f.update_coefficients();
        f
    }

    /// Actualiza la tasa de muestreo preservando el estado interno.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = (sample_rate as f64).max(1.0);
        self.update_coefficients();
    }

    /// Reinicia los registros de retardo a cero.
    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Configura tipo, frecuencia (`Hz`), factor `Q` y ganancia (`dB`).
    pub fn set_params(&mut self, kind: MatchedFilterKind, freq_hz: f32, q: f32, gain_db: f32) {
        self.kind = kind;
        let max_f = 0.495 * self.sample_rate;
        self.freq_hz = (freq_hz as f64).clamp(5.0, max_f.max(10.0));
        self.q = (q as f64).clamp(0.1, 20.0);
        self.gain_db = if gain_db.is_finite() {
            (gain_db as f64).clamp(-36.0, 36.0)
        } else {
            0.0
        };
        self.update_coefficients();
    }

    /// Resuelve `(b0, b1, b2)` desde `(B0, B1, B2)` en la parametrización de Vicanek (fase mínima).
    #[inline]
    fn solve_numerator_from_vicanek(big_b0: f64, big_b1: f64, big_b2: f64) -> (f64, f64, f64) {
        let sqrt_b0 = big_b0.max(0.0).sqrt();
        let sqrt_b1 = big_b1.max(0.0).sqrt();
        let w = 0.5 * (sqrt_b0 + sqrt_b1);
        let b1 = 0.5 * (sqrt_b0 - sqrt_b1);
        let disc = (w * w + big_b2).max(0.0).sqrt();
        let b0 = 0.5 * (w + disc);
        let b2 = if b0.abs() > 1e-18 {
            -big_b2 / (4.0 * b0)
        } else {
            0.5 * (w - disc)
        };
        (b0, b1, b2)
    }

    /// Calcula los polos por invariancia al impulso del prototipo `s^2 + (w0/q)*s + w0^2`.
    #[inline]
    fn matched_poles(w0: f64, q_eff: f64) -> (f64, f64) {
        let zeta = 0.5 / q_eff.max(1e-6);
        let a2 = (-2.0 * w0 * zeta).exp();
        let a1 = if zeta <= 1.0 {
            -2.0 * (-w0 * zeta).exp() * (w0 * (1.0 - zeta * zeta).sqrt()).cos()
        } else {
            -2.0 * (-w0 * zeta).exp() * (w0 * (zeta * zeta - 1.0).sqrt()).cosh()
        };
        (a1, a2)
    }

    fn update_coefficients(&mut self) {
        let w0 = (2.0 * PI * self.freq_hz / self.sample_rate).clamp(1e-4, PI - 1e-4);

        match self.kind {
            MatchedFilterKind::Peaking => {
                if self.gain_db.abs() < 1e-5 {
                    self.b0 = 1.0;
                    self.b1 = 0.0;
                    self.b2 = 0.0;
                    self.a1 = 0.0;
                    self.a2 = 0.0;
                    return;
                }
                let is_cut = self.gain_db < 0.0;
                // Diseñamos siempre boost >= 1 y para corte invertimos exacto H_cut(z) = 1 / H_boost(z).
                let g = 10.0_f64.powf(self.gain_db.abs() / 20.0);
                let q_den = self.q * g.sqrt();
                let (a1, a2) = Self::matched_poles(w0, q_den);

                let big_a0 = (1.0 + a1 + a2).powi(2);
                let big_a1 = (1.0 - a1 + a2).powi(2);
                let big_a2 = -4.0 * a2;

                let phi0 = (0.5 * w0).cos().powi(2);
                let phi1 = 1.0 - phi0;
                let phi2 = 4.0 * phi0 * phi1;

                // Ganancia analógica exacta en Nyquist (Omega_nyq = pi / w0).
                let om_nyq_sq = (PI / w0).powi(2);
                let one_m_om_sq = (1.0 - om_nyq_sq).powi(2);
                let q_sq = self.q * self.q;
                let r1 =
                    (one_m_om_sq + g * om_nyq_sq / q_sq) / (one_m_om_sq + om_nyq_sq / (g * q_sq));

                let big_b0 = big_a0;
                let big_b1 = r1 * big_a1;
                let a_at_w0 = big_a0 * phi0 + big_a1 * phi1 + big_a2 * phi2;
                let big_b2 = (g * g * a_at_w0 - big_b0 * phi0 - big_b1 * phi1) / phi2.max(1e-12);

                let (b0, b1, b2) = Self::solve_numerator_from_vicanek(big_b0, big_b1, big_b2);
                if is_cut {
                    self.set_inverted(b0, b1, b2, a1, a2);
                } else {
                    self.b0 = b0;
                    self.b1 = b1;
                    self.b2 = b2;
                    self.a1 = a1;
                    self.a2 = a2;
                }
            }
            MatchedFilterKind::LowShelf => {
                if self.gain_db.abs() < 1e-5 {
                    self.b0 = 1.0;
                    self.b1 = 0.0;
                    self.b2 = 0.0;
                    self.a1 = 0.0;
                    self.a2 = 0.0;
                    return;
                }
                let is_cut = self.gain_db < 0.0;
                let g = 10.0_f64.powf(self.gain_db.abs() / 20.0);
                let a_half = g.sqrt();
                let wp = (w0 / a_half.sqrt()).clamp(1e-4, PI - 1e-4);
                let (a1, a2) = Self::matched_poles(wp, self.q);

                let big_a0 = (1.0 + a1 + a2).powi(2);
                let big_a1 = (1.0 - a1 + a2).powi(2);
                let big_a2 = -4.0 * a2;

                let phi0 = (0.5 * w0).cos().powi(2);
                let phi1 = 1.0 - phi0;
                let phi2 = 4.0 * phi0 * phi1;

                // Prototipo analógico H_LS(s) = A*(s^2 + (sqrt(A)/Q)w0 s + A w0^2) / (A s^2 + (sqrt(A)/Q)w0 s + w0^2)
                let u = PI / w0;
                let u2 = u * u;
                let bw_term = a_half * u2 / (self.q * self.q);
                let r1 =
                    g * ((a_half - u2).powi(2) + bw_term) / ((1.0 - a_half * u2).powi(2) + bw_term);

                let big_b0 = g * g * big_a0;
                let big_b1 = r1 * big_a1;
                let a_at_w0 = big_a0 * phi0 + big_a1 * phi1 + big_a2 * phi2;
                // En w0 la magnitud es sqrt(G) => |H|^2 = G.
                let big_b2 = (g * a_at_w0 - big_b0 * phi0 - big_b1 * phi1) / phi2.max(1e-12);

                let (b0, b1, b2) = Self::solve_numerator_from_vicanek(big_b0, big_b1, big_b2);
                if is_cut {
                    self.set_inverted(b0, b1, b2, a1, a2);
                } else {
                    self.b0 = b0;
                    self.b1 = b1;
                    self.b2 = b2;
                    self.a1 = a1;
                    self.a2 = a2;
                }
            }
            MatchedFilterKind::HighShelf => {
                if self.gain_db.abs() < 1e-5 {
                    self.b0 = 1.0;
                    self.b1 = 0.0;
                    self.b2 = 0.0;
                    self.a1 = 0.0;
                    self.a2 = 0.0;
                    return;
                }
                let is_cut = self.gain_db < 0.0;
                let g = 10.0_f64.powf(self.gain_db.abs() / 20.0);
                let a_half = g.sqrt();
                let wp = (w0 * a_half.sqrt()).clamp(1e-4, PI - 1e-4);
                let (a1, a2) = Self::matched_poles(wp, self.q);

                let big_a0 = (1.0 + a1 + a2).powi(2);
                let big_a1 = (1.0 - a1 + a2).powi(2);
                let big_a2 = -4.0 * a2;

                let phi0 = (0.5 * w0).cos().powi(2);
                let phi1 = 1.0 - phi0;
                let phi2 = 4.0 * phi0 * phi1;

                // Prototipo analógico H_HS(s) = A*(A s^2 + (sqrt(A)/Q)w0 s + w0^2) / (s^2 + (sqrt(A)/Q)w0 s + A w0^2)
                let u = PI / w0;
                let u2 = u * u;
                let bw_term = a_half * u2 / (self.q * self.q);
                let r1 =
                    g * ((1.0 - a_half * u2).powi(2) + bw_term) / ((a_half - u2).powi(2) + bw_term);

                let big_b0 = big_a0;
                let big_b1 = r1 * big_a1;
                let a_at_w0 = big_a0 * phi0 + big_a1 * phi1 + big_a2 * phi2;
                // En w0 la magnitud es sqrt(G) => |H|^2 = G.
                let big_b2 = (g * a_at_w0 - big_b0 * phi0 - big_b1 * phi1) / phi2.max(1e-12);

                let (b0, b1, b2) = Self::solve_numerator_from_vicanek(big_b0, big_b1, big_b2);
                if is_cut {
                    self.set_inverted(b0, b1, b2, a1, a2);
                } else {
                    self.b0 = b0;
                    self.b1 = b1;
                    self.b2 = b2;
                    self.a1 = a1;
                    self.a2 = a2;
                }
            }
            MatchedFilterKind::LowPass => {
                // Vicanek Matched Low-Pass (eq. 12–16): |H(0)| = 1, |H(pi)| = R1, |H(w0)| = Q.
                let (a1, a2) = Self::matched_poles(w0, self.q);
                let big_a0 = (1.0 + a1 + a2).powi(2);
                let big_a1 = (1.0 - a1 + a2).powi(2);
                let big_a2 = -4.0 * a2;
                let r1 = big_a0 * self.q * self.q
                    / (big_a0
                        + ((PI / w0).powi(2) - 1.0).powi(2) * (self.q * self.q)
                        + (PI / w0).powi(2))
                    .max(1e-12);
                let big_b0 = big_a0;
                let big_b1 = (r1 * big_a1 / big_a0.max(1e-12)).clamp(0.0, big_a1);
                let phi0 = (0.5 * w0).cos().powi(2);
                let phi1 = 1.0 - phi0;
                let phi2 = 4.0 * phi0 * phi1;
                let a_at_w0 = big_a0 * phi0 + big_a1 * phi1 + big_a2 * phi2;
                let big_b2 =
                    (self.q * self.q * a_at_w0 - big_b0 * phi0 - big_b1 * phi1) / phi2.max(1e-12);
                let (b0, b1, b2) = Self::solve_numerator_from_vicanek(big_b0, big_b1, big_b2);
                self.b0 = b0;
                self.b1 = b1;
                self.b2 = b2;
                self.a1 = a1;
                self.a2 = a2;
            }
            MatchedFilterKind::HighPass => {
                // Vicanek Matched High-Pass: B0 = B2 = 0 => b0 = sqrt(B1)/4, b1 = -2*b0, b2 = b0 con cero doble en DC.
                let (a1, a2) = Self::matched_poles(w0, self.q);
                let big_a0 = (1.0 + a1 + a2).powi(2);
                let big_a1 = (1.0 - a1 + a2).powi(2);
                let big_a2 = -4.0 * a2;
                let phi0 = (0.5 * w0).cos().powi(2);
                let phi1 = 1.0 - phi0;
                let phi2 = 4.0 * phi0 * phi1;
                let u = PI / w0;
                let r_nyq = (u.powi(4)) / ((1.0 - u * u).powi(2) + (u / self.q).powi(2));
                let a_at_w0 = big_a0 * phi0 + big_a1 * phi1 + big_a2 * phi2;
                let r1_w0 = (self.q * self.q * a_at_w0 / (4.0 * phi1 * phi1).max(1e-12))
                    .max(r_nyq * big_a1 / 16.0);
                let b0 = r1_w0.sqrt() * 0.5;
                self.b0 = b0;
                self.b1 = -2.0 * b0;
                self.b2 = b0;
                self.a1 = a1;
                self.a2 = a2;
            }
            MatchedFilterKind::BandPass => {
                // Vicanek Matched Band-Pass (B0 = 0 => b0 + b1 + b2 = 0, pico unitario en w0).
                let (a1, a2) = Self::matched_poles(w0, self.q);
                let big_a0 = (1.0 + a1 + a2).powi(2);
                let big_a1 = (1.0 - a1 + a2).powi(2);
                let big_a2 = -4.0 * a2;
                let phi0 = (0.5 * w0).cos().powi(2);
                let phi1 = 1.0 - phi0;
                let phi2 = 4.0 * phi0 * phi1;
                let a_at_w0 = big_a0 * phi0 + big_a1 * phi1 + big_a2 * phi2;
                let u = PI / w0;
                let r_nyq = (u / self.q).powi(2) / ((1.0 - u * u).powi(2) + (u / self.q).powi(2));
                let big_b1 = r_nyq * big_a1;
                let big_b2 = (a_at_w0 - big_b1 * phi1) / phi2.max(1e-12);
                let (mut b0, b1, b2) = Self::solve_numerator_from_vicanek(0.0, big_b1, big_b2);
                // Normalizamos pico en w0 exactamente a 1.0 para que el paso banda mantenga ganancia 0 dB.
                let b_w0 = (big_b1 * phi1 + big_b2 * phi2).max(1e-18).sqrt();
                let scale = a_at_w0.sqrt() / b_w0;
                b0 *= scale;
                self.b0 = b0;
                self.b1 = b1 * scale;
                self.b2 = b2 * scale;
                self.a1 = a1;
                self.a2 = a2;
            }
        }
    }

    #[inline]
    fn set_inverted(&mut self, b0: f64, b1: f64, b2: f64, a1: f64, a2: f64) {
        let inv = 1.0 / b0.max(1e-12);
        self.b0 = inv;
        self.b1 = a1 * inv;
        self.b2 = a2 * inv;
        self.a1 = b1 * inv;
        self.a2 = b2 * inv;
    }

    /// Procesa una muestra en `f64` usando Direct Form II Transposed.
    #[inline]
    pub fn process_sample_f64(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        let z1_next = self.b1 * x - self.a1 * y + self.z2;
        let z2_next = self.b2 * x - self.a2 * y;
        self.z1 = flush_denormal_f64(z1_next);
        self.z2 = flush_denormal_f64(z2_next);
        y
    }

    /// Procesa una muestra en `f32`.
    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let x = if input.is_finite() { input as f64 } else { 0.0 };
        self.process_sample_f64(x) as f32
    }

    /// Magnitud lineal exacta de `H(e^{jw})` a `freq_hz`.
    pub fn magnitude_response(&self, freq_hz: f32) -> f32 {
        let f = (freq_hz as f64).clamp(0.0, 0.5 * self.sample_rate);
        let w = 2.0 * PI * f / self.sample_rate;
        let cw = w.cos();
        let sw = w.sin();
        let c2w = (2.0 * w).cos();
        let s2w = (2.0 * w).sin();

        let num_re = self.b0 + self.b1 * cw + self.b2 * c2w;
        let num_im = -(self.b1 * sw + self.b2 * s2w);
        let den_re = 1.0 + self.a1 * cw + self.a2 * c2w;
        let den_im = -(self.a1 * sw + self.a2 * s2w);

        let num = (num_re * num_re + num_im * num_im).sqrt();
        let den = (den_re * den_re + den_im * den_im).sqrt().max(1e-18);
        (num / den) as f32
    }

    /// Magnitud en decibelios a `freq_hz`.
    #[inline]
    pub fn magnitude_db_at(&self, freq_hz: f32) -> f32 {
        crate::lin_to_db(self.magnitude_response(freq_hz))
    }
}
