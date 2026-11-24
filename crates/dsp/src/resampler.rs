use crate::sanitize_sample;

/// Remuestreador streaming en tiempo real sin asignaciones de memoria.
///
/// Usa interpolación cúbica de Hermite de 4 puntos entre muestras consecutivas de entrada.
#[derive(Debug, Clone)]
pub struct Resampler {
    in_rate: u32,
    out_rate: u32,
    step: f64,
    pos: f64,
    y0: f32,
    y1: f32,
    y2: f32,
    y3: f32,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_r = in_rate.max(1);
        let out_r = out_rate.max(1);
        Self {
            in_rate: in_r,
            out_rate: out_r,
            step: in_r as f64 / out_r as f64,
            pos: 0.0,
            y0: 0.0,
            y1: 0.0,
            y2: 0.0,
            y3: 0.0,
        }
    }

    #[inline]
    pub fn is_passthrough(&self) -> bool {
        self.in_rate == self.out_rate
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.y0 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
        self.y3 = 0.0;
    }

    /// Procesa una muestra a `in_rate` y emite cero o más muestras a `out_rate`.
    #[inline]
    pub fn process_sample<F: FnMut(f32)>(&mut self, x: f32, mut emit: F) {
        let x = sanitize_sample(x);
        if self.in_rate == self.out_rate {
            emit(x);
            return;
        }

        self.y0 = self.y1;
        self.y1 = self.y2;
        self.y2 = self.y3;
        self.y3 = x;

        while self.pos < 1.0 {
            let t = self.pos as f32;
            let c0 = self.y1;
            let c1 = 0.5 * (self.y2 - self.y0);
            let c2 = self.y0 - 2.5 * self.y1 + 2.0 * self.y2 - 0.5 * self.y3;
            let c3 = 0.5 * (self.y3 - self.y0) + 1.5 * (self.y1 - self.y2);
            let out = ((c3 * t + c2) * t + c1) * t + c0;
            emit(if out.is_finite() { out } else { 0.0 });
            self.pos += self.step;
        }
        self.pos -= 1.0;
    }
}
