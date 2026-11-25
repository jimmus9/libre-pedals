//! Utilidades compartidas de retardo fraccional y filtros *all-pass* para modulación, delay y reverb.
//!
//! - [`FractionalDelayLine`]: búfer circular potencia de 2 con interpolación cúbica de Hermite de 4 puntos
//!   (Laakso et al., IEEE SPM 1996), libre de realimentación interna y estable bajo modulación rápida.
//! - [`OnePoleAllPassTpt`]: filtro *all-pass* de 1.er orden en topología TPT (Zavalishin), estable
//!   al barrer frecuencias por LFO (Phaser) o en cascadas dispersivas (Spring Reverb).
//! - [`SchroederAllPass`]: difusor *all-pass* discreto de ganancia unitaria para tanques de reverberación.

use std::f64::consts::PI;

use crate::denormals::{flush_denormal, flush_denormal_f64};
use crate::sanitize_sample;

/// Línea de retardo circular potencia de 2 prealojada en `prepare()`.
#[derive(Debug, Clone)]
pub struct FractionalDelayLine {
    buf: Vec<f32>,
    mask: usize,
    write_pos: usize,
}

impl Default for FractionalDelayLine {
    fn default() -> Self {
        Self::new()
    }
}

impl FractionalDelayLine {
    pub fn new() -> Self {
        let mut s = Self {
            buf: Vec::new(),
            mask: 0,
            write_pos: 0,
        };
        s.prepare(64);
        s
    }

    pub fn prepare(&mut self, max_delay_samples: usize) {
        let cap = (max_delay_samples + 8).next_power_of_two().max(16);
        self.buf.resize(cap, 0.0);
        self.mask = cap - 1;
        self.reset();
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.write_pos = 0;
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buf[self.write_pos] = flush_denormal(sanitize_sample(x));
        self.write_pos = (self.write_pos + 1) & self.mask;
    }

    /// Lee `delay_samples` muestras atrás *antes* de llamar a [`Self::push`] en el paso actual.
    #[inline]
    pub fn read_before_push_int(&self, delay_samples: usize) -> f32 {
        let cap = self.buf.len();
        let d = delay_samples.clamp(1, cap.saturating_sub(2));
        self.buf[(self.write_pos + cap - d) & self.mask]
    }

    /// Lee un retardo fraccional (Hermite de 4 puntos) *antes* de llamar a [`Self::push`].
    #[inline]
    pub fn read_before_push_hermite(&self, delay_samples: f64) -> f32 {
        let cap = self.buf.len();
        let max_d = (cap.saturating_sub(4)).max(2) as f64;
        let d = delay_samples.clamp(2.0, max_d);
        let d_int = d.floor() as usize;
        let t = (d - d_int as f64) as f32;

        let y0 = self.buf[(self.write_pos + cap - (d_int - 1)) & self.mask];
        let y1 = self.buf[(self.write_pos + cap - d_int) & self.mask];
        let y2 = self.buf[(self.write_pos + cap - (d_int + 1)) & self.mask];
        let y3 = self.buf[(self.write_pos + cap - (d_int + 2)) & self.mask];

        hermite4(y0, y1, y2, y3, t)
    }

    /// Lee un retardo fraccional (Hermite de 4 puntos) *después* de llamar a [`Self::push`].
    #[inline]
    pub fn read_after_push_hermite(&self, delay_samples: f64) -> f32 {
        let cap = self.buf.len();
        let max_d = (cap.saturating_sub(4)).max(1) as f64;
        let d = delay_samples.clamp(1.0, max_d);
        let d_int = d.floor() as usize;
        let t = (d - d_int as f64) as f32;

        let y0 = self.buf[(self.write_pos + cap - d_int) & self.mask];
        let y1 = self.buf[(self.write_pos + cap - (d_int + 1)) & self.mask];
        let y2 = self.buf[(self.write_pos + cap - (d_int + 2)) & self.mask];
        let y3 = self.buf[(self.write_pos + cap - (d_int + 3)) & self.mask];

        hermite4(y0, y1, y2, y3, t)
    }
}

#[inline]
fn hermite4(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// Filtro *all-pass* de 1.er orden en topología trapezoidal preservada (TPT/ZDF).
#[derive(Debug, Clone, Copy)]
pub struct OnePoleAllPassTpt {
    g_cap: f64,
    s: f64,
}

impl Default for OnePoleAllPassTpt {
    fn default() -> Self {
        Self::new()
    }
}

impl OnePoleAllPassTpt {
    pub fn new() -> Self {
        let mut ap = Self { g_cap: 0.1, s: 0.0 };
        ap.set_cutoff(1_000.0, 48_000.0);
        ap
    }

    pub fn reset(&mut self) {
        self.s = 0.0;
    }

    #[inline]
    pub fn set_cutoff(&mut self, fc_hz: f64, sr: f64) {
        let sr_safe = sr.max(1.0);
        let fc = fc_hz.clamp(10.0, 0.47 * sr_safe);
        let g = (PI * fc / sr_safe).tan();
        self.g_cap = g / (1.0 + g);
    }

    #[inline]
    pub fn process_sample(&mut self, x: f64) -> f64 {
        let v = self.g_cap * (x - self.s);
        let lp = v + self.s;
        self.s = flush_denormal_f64(lp + v);
        2.0 * lp - x
    }
}

/// Difusor *all-pass* clásico de Schroeder para líneas de reverberación densas.
#[derive(Debug, Clone)]
pub struct SchroederAllPass {
    buf: Vec<f32>,
    len: usize,
    pos: usize,
    coeff: f32,
}

impl Default for SchroederAllPass {
    fn default() -> Self {
        Self::new()
    }
}

impl SchroederAllPass {
    pub fn new() -> Self {
        let mut s = Self {
            buf: Vec::new(),
            len: 1,
            pos: 0,
            coeff: 0.5,
        };
        s.prepare(16, 0.5);
        s
    }

    pub fn prepare(&mut self, delay_samples: usize, coeff: f32) {
        let len = delay_samples.max(1);
        self.buf.resize(len, 0.0);
        self.len = len;
        self.coeff = coeff.clamp(-0.9, 0.9);
        self.reset();
    }

    pub fn reset(&mut self) {
        self.buf.fill(0.0);
        self.pos = 0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let d = self.buf[self.pos];
        let w = flush_denormal(x - self.coeff * d);
        self.buf[self.pos] = w;
        self.pos += 1;
        if self.pos >= self.len {
            self.pos = 0;
        }
        d + self.coeff * w
    }
}
