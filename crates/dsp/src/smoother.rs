//! Utilidades de suavizado temporal sin asignación de memoria para parámetros continuos y discretos.

use std::f64::consts::PI;

/// Suavizador exponencial de 1.er orden en `f64` para evitar *zipper noise* al mover perillas.
#[derive(Debug, Clone, Copy)]
pub struct OnePoleSmoother {
    current: f64,
    target: f64,
    coeff: f64,
    tau_ms: f64,
    sample_rate: f64,
}

impl OnePoleSmoother {
    /// Crea un suavizador inicializado en `initial` con constante de tiempo `tau_ms` (p. ej. 5–20 ms).
    pub fn new(tau_ms: f32, sample_rate: f32, initial: f32) -> Self {
        let mut s = Self {
            current: initial as f64,
            target: initial as f64,
            coeff: 0.0,
            tau_ms: (tau_ms as f64).max(0.05),
            sample_rate: (sample_rate as f64).max(1.0),
        };
        s.recompute_coeff();
        s
    }

    #[inline]
    fn recompute_coeff(&mut self) {
        let tau_s = (self.tau_ms * 1e-3).max(1e-5);
        self.coeff = (-1.0 / (tau_s * self.sample_rate)).exp();
    }

    /// Actualiza la frecuencia de muestreo preservando la constante de tiempo en milisegundos.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = (sample_rate as f64).max(1.0);
        self.recompute_coeff();
    }

    /// Ajusta la constante de tiempo `tau_ms` en milisegundos.
    pub fn set_tau_ms(&mut self, tau_ms: f32) {
        self.tau_ms = (tau_ms as f64).max(0.05);
        self.recompute_coeff();
    }

    /// Fija un nuevo objetivo de parámetro sin reiniciar el valor actual.
    #[inline]
    pub fn set_target(&mut self, target: f32) {
        if target.is_finite() {
            self.target = target as f64;
        }
    }

    /// Reinicia instantáneamente tanto el estado actual como el objetivo (útil en `prepare()`/`reset()`).
    #[inline]
    pub fn reset(&mut self, value: f32) {
        let v = if value.is_finite() { value as f64 } else { 0.0 };
        self.current = v;
        self.target = v;
    }

    /// Avanza una muestra y devuelve el valor suavizado en `f64`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        self.current = self.target + self.coeff * (self.current - self.target);
        if (self.current - self.target).abs() < 1e-12 {
            self.current = self.target;
        }
        self.current
    }

    /// Avanza una muestra y devuelve el valor suavizado en `f32`.
    #[inline]
    pub fn next_val(&mut self) -> f32 {
        self.next_f64() as f32
    }

    /// Valor actual sin avanzar el filtro.
    #[inline]
    pub fn current(&self) -> f32 {
        self.current as f32
    }

    /// Valor objetivo actual.
    #[inline]
    pub fn target(&self) -> f32 {
        self.target as f32
    }

    /// Indica si el suavizador ya convergió al objetivo dentro de tolerancia de audio.
    #[inline]
    pub fn is_settled(&self) -> bool {
        (self.current - self.target).abs() <= 1e-6
    }
}

/// Gestor de *crossfade* suave (`5–10 ms`) para selectores discretos [`crate::ParamScale::Stepped`].
///
/// Evita clics y discontinuidades de fase al cambiar de modo (`Op-Amp` vs `JFET`, tipos de
/// reverb/delay, etc.) interpolando con ventana coseno entre la rama anterior y la nueva.
#[derive(Debug, Clone, Copy)]
pub struct ModeCrossfader {
    prev_mode: usize,
    target_mode: usize,
    progress: f64,
    step: f64,
    fade_ms: f64,
    sample_rate: f64,
    active: bool,
}

impl ModeCrossfader {
    /// Crea un conmutador con duración de fundido `fade_ms` (típicamente `5.0..=10.0` ms).
    pub fn new(fade_ms: f32, sample_rate: f32, initial_mode: usize) -> Self {
        let mut c = Self {
            prev_mode: initial_mode,
            target_mode: initial_mode,
            progress: 1.0,
            step: 0.0,
            fade_ms: (fade_ms as f64).clamp(0.5, 100.0),
            sample_rate: (sample_rate as f64).max(1.0),
            active: false,
        };
        c.recompute_step();
        c
    }

    #[inline]
    fn recompute_step(&mut self) {
        let fade_samples = (self.fade_ms * 1e-3 * self.sample_rate).max(1.0);
        self.step = 1.0 / fade_samples;
    }

    /// Actualiza la tasa de muestreo conservando el modo activo.
    pub fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = (sample_rate as f64).max(1.0);
        self.recompute_step();
    }

    /// Cambia el modo objetivo disparando un *crossfade* desde el modo actualmente asentado.
    pub fn set_mode(&mut self, mode: usize) {
        if mode == self.target_mode {
            return;
        }
        if !self.active {
            self.prev_mode = self.target_mode;
            self.progress = 0.0;
        } else if mode == self.prev_mode {
            // Reversión limpia si el usuario vuelve rápido al modo previo mitad de camino.
            self.prev_mode = self.target_mode;
            self.progress = (1.0 - self.progress).clamp(0.0, 1.0);
        } else {
            // Toma el modo dominante hasta ese instante como punto de partida.
            self.prev_mode = if self.progress >= 0.5 {
                self.target_mode
            } else {
                self.prev_mode
            };
            self.progress = 0.0;
        }
        self.target_mode = mode;
        self.active = true;
    }

    /// Fuerza un modo de forma instantánea sin fundido (útil en `prepare()`/`reset()`).
    pub fn reset(&mut self, mode: usize) {
        self.prev_mode = mode;
        self.target_mode = mode;
        self.progress = 1.0;
        self.active = false;
    }

    /// Modo objetivo actual.
    #[inline]
    pub fn active_mode(&self) -> usize {
        self.target_mode
    }

    /// Devuelve `(modo_anterior, modo_objetivo)` para evaluar ambas ramas solo cuando es necesario.
    #[inline]
    pub fn pair(&self) -> (usize, usize) {
        (self.prev_mode, self.target_mode)
    }

    /// Indica si hay una transición en curso.
    #[inline]
    pub fn is_crossfading(&self) -> bool {
        self.active
    }

    /// Avanza una muestra y devuelve las ganancias `(prev_gain, next_gain)` cuya suma es `1.0`.
    #[inline]
    pub fn advance_gains(&mut self) -> (f32, f32) {
        if !self.active {
            return (0.0, 1.0);
        }
        let w = 0.5 * (1.0 - (PI * self.progress).cos());
        let next_gain = w.clamp(0.0, 1.0);
        let prev_gain = 1.0 - next_gain;
        self.progress += self.step;
        if self.progress >= 1.0 {
            self.progress = 1.0;
            self.prev_mode = self.target_mode;
            self.active = false;
        }
        (prev_gain as f32, next_gain as f32)
    }

    /// Avanza una muestra y mezcla ambas salidas sin clics.
    #[inline]
    pub fn mix_sample(&mut self, prev_sample: f32, next_sample: f32) -> f32 {
        let (g_prev, g_next) = self.advance_gains();
        prev_sample * g_prev + next_sample * g_next
    }
}
