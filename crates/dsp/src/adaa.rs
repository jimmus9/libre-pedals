//! Antialiasing mediante derivadas analíticas de primer orden (**ADAA1**, Parker et al., DAFx 2016)
//! calculado íntegramente en doble precisión (`f64`) para evitar errores de cancelación cuando `x_n ≈ x_{n-1}`.

use std::f64::consts::LN_2;

/// Umbral por debajo del cual se usa evaluación directa en el punto medio en lugar de cociente incremental.
const ADAA_EPS: f64 = 1.0e-5;

/// Retardo exacto de media muestra (`0.5 * (x[n] + x[n-1])`) para alinear una rama seca paralela
/// con una rama no lineal procesada mediante ADAA1.
#[derive(Debug, Clone, Copy, Default)]
pub struct HalfSampleDelay {
    prev: f64,
}

impl HalfSampleDelay {
    #[inline]
    pub fn reset(&mut self) {
        self.prev = 0.0;
    }

    #[inline]
    pub fn process_sample(&mut self, x: f64) -> f64 {
        let y = 0.5 * (x + self.prev);
        self.prev = x;
        y
    }
}

/// Antiderivada numéricamente estable `F1(u) = ln(cosh(u))` sin desbordamiento para `|u|` grande.
#[inline]
pub fn ln_cosh_stable(u: f64) -> f64 {
    let a = u.abs();
    a + (-2.0 * a).exp().ln_1p() - LN_2
}

/// Evaluador ADAA1 de `tanh(x)` en `f64` con latencia de 0.5 muestras.
#[derive(Debug, Clone, Copy, Default)]
pub struct TanhAdaa1 {
    prev_x: f64,
    prev_f1: f64,
}

impl TanhAdaa1 {
    /// Reinicia el estado interno (x_{-1} = 0).
    #[inline]
    pub fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_f1 = 0.0;
    }

    /// Procesa una muestra `x` aplicando `(F1(x_n) - F1(x_{n-1})) / (x_n - x_{n-1})`.
    #[inline]
    pub fn process_sample(&mut self, x: f64) -> f64 {
        let f1 = ln_cosh_stable(x);
        let dx = x - self.prev_x;
        let out = if dx.abs() < ADAA_EPS {
            (0.5 * (x + self.prev_x)).tanh()
        } else {
            (f1 - self.prev_f1) / dx
        };
        self.prev_x = x;
        self.prev_f1 = f1;
        out
    }
}

/// Recorte suave asimétrico C1 continuo con ADAA1 en `f64`:
///
/// - Para `x >= 0`: `pos_limit * tanh(x / pos_limit)`
/// - Para `x < 0`:  `neg_limit * tanh(x / neg_limit)`
///
/// Cuando `pos_limit == neg_limit` genera exclusivamente armónicos impares; cuando difieren,
/// añade contenido armónico par de forma musical y continua en `x = 0`.
#[derive(Debug, Clone, Copy)]
pub struct AsymSoftClipAdaa1 {
    pos_limit: f64,
    neg_limit: f64,
    prev_x: f64,
    prev_f1: f64,
}

impl Default for AsymSoftClipAdaa1 {
    fn default() -> Self {
        Self::new(1.0, 1.0)
    }
}

impl AsymSoftClipAdaa1 {
    /// Crea el waveshaper con límites positivos y negativos (> 0.05).
    pub fn new(pos_limit: f64, neg_limit: f64) -> Self {
        Self {
            pos_limit: pos_limit.max(0.05),
            neg_limit: neg_limit.max(0.05),
            prev_x: 0.0,
            prev_f1: 0.0,
        }
    }

    /// Actualiza los techos asimétricos sin perder el historial de muestra previa.
    #[inline]
    pub fn set_limits(&mut self, pos_limit: f64, neg_limit: f64) {
        self.pos_limit = pos_limit.max(0.05);
        self.neg_limit = neg_limit.max(0.05);
        self.prev_f1 = self.antiderivative(self.prev_x);
    }

    #[inline]
    pub fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_f1 = 0.0;
    }

    #[inline]
    fn shape(&self, x: f64) -> f64 {
        if x >= 0.0 {
            self.pos_limit * (x / self.pos_limit).tanh()
        } else {
            self.neg_limit * (x / self.neg_limit).tanh()
        }
    }

    #[inline]
    fn antiderivative(&self, x: f64) -> f64 {
        if x >= 0.0 {
            self.pos_limit * self.pos_limit * ln_cosh_stable(x / self.pos_limit)
        } else {
            self.neg_limit * self.neg_limit * ln_cosh_stable(x / self.neg_limit)
        }
    }

    /// Procesa una muestra con ADAA1.
    #[inline]
    pub fn process_sample(&mut self, x: f64) -> f64 {
        let f1 = self.antiderivative(x);
        let dx = x - self.prev_x;
        let out = if dx.abs() < ADAA_EPS {
            self.shape(0.5 * (x + self.prev_x))
        } else {
            (f1 - self.prev_f1) / dx
        };
        self.prev_x = x;
        self.prev_f1 = f1;
        out
    }
}
