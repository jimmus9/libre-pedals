use std::any::Any;

pub mod gain;
pub mod gate;
pub mod overdrive;

pub use gain::Gain;
pub use gate::NoiseGate;
pub use overdrive::Overdrive;

/// Conversión de decibelios a ganancia lineal. Valores <= -120 dB se tratan como silencio.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    if !db.is_finite() || db <= -120.0 {
        0.0
    } else {
        10.0_f32.powf(db * 0.05)
    }
}

/// Conversión de amplitud lineal a dBFS (mínimo -120 dBFS).
#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    if !lin.is_finite() || lin <= 1e-6 {
        -120.0
    } else {
        20.0 * lin.log10()
    }
}

/// Limpia muestras no finitas y acota valores extremos para evitar NaN/Inf en filtros IIR.
#[inline]
pub(crate) fn sanitize_sample(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(-1e12, 1e12)
    } else {
        0.0
    }
}

/// Trait auxiliar para permitir downcast seguro desde `&mut dyn Effect` sin requerir boilerplate.
pub trait AsAny {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T: Any> AsAny for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Interfaz común para todos los efectos de audio en tiempo real.
///
/// Ninguna implementación debe reservar memoria, bloquear mutexes ni hacer E/S en `process`.
pub trait Effect: Send + AsAny {
    fn prepare(&mut self, sample_rate: f32, max_block: usize);
    fn process(&mut self, buf: &mut [f32]);
    fn reset(&mut self) {}
}

/// Filtro pasa-altos de primer orden (bloqueador de componente DC).
///
/// Ecuación: `y[n] = x[n] - x[n-1] + R * y[n-1]`.
#[derive(Debug, Clone)]
pub struct DcBlocker {
    cutoff_hz: f32,
    r: f32,
    x1: f32,
    y1: f32,
}

impl DcBlocker {
    pub fn new(cutoff_hz: f32) -> Self {
        let mut s = Self {
            cutoff_hz: cutoff_hz.clamp(1.0, 200.0),
            r: 0.998,
            x1: 0.0,
            y1: 0.0,
        };
        s.prepare(48_000.0);
        s
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        self.r = (-2.0 * std::f32::consts::PI * self.cutoff_hz / sr)
            .exp()
            .clamp(0.5, 0.9999);
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
    }

    #[inline]
    pub fn process_sample(&mut self, x: f32) -> f32 {
        let x = sanitize_sample(x);
        let y = (x - self.x1) + self.r * self.y1;
        if !y.is_finite() {
            self.x1 = 0.0;
            self.y1 = 0.0;
            return 0.0;
        }
        self.x1 = x;
        self.y1 = y;
        y
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = self.process_sample(*s);
        }
    }
}

/// Cadena ordenada de efectos con estado de bypass individual por pedal.
pub struct Chain {
    slots: Vec<(Box<dyn Effect>, bool)>,
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

impl Chain {
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Agrega un efecto al final de la cadena. `bypass = false` significa activo.
    pub fn push<E: Effect + 'static>(&mut self, effect: E, bypass: bool) -> usize {
        let idx = self.slots.len();
        self.slots.push((Box::new(effect), bypass));
        idx
    }

    pub fn set_bypass(&mut self, index: usize, bypass: bool) {
        if let Some(slot) = self.slots.get_mut(index) {
            slot.1 = bypass;
        }
    }

    pub fn is_bypassed(&self, index: usize) -> Option<bool> {
        self.slots.get(index).map(|s| s.1)
    }

    /// Acceso tipado al efecto en `index` para actualizar parámetros sin reservas de memoria.
    pub fn effect_mut<T: Effect + 'static>(&mut self, index: usize) -> Option<&mut T> {
        self.slots
            .get_mut(index)
            .and_then(|(fx, _)| fx.as_any_mut().downcast_mut::<T>())
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

impl Effect for Chain {
    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        for (fx, _) in &mut self.slots {
            fx.prepare(sample_rate, max_block);
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for (fx, bypass) in &mut self.slots {
            if !*bypass {
                fx.process(buf);
            }
        }
    }

    fn reset(&mut self) {
        for (fx, _) in &mut self.slots {
            fx.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_zero_db_is_identity() {
        let mut gain = Gain::new(0.0);
        gain.prepare(48_000.0, 256);

        let original: Vec<f32> = (0..256)
            .map(|i| (i as f32 * 0.123).sin() * 0.75)
            .collect();
        let mut buf = original.clone();
        gain.process(&mut buf);

        for (a, b) in buf.iter().zip(original.iter()) {
            assert!((a - b).abs() < 1e-7, "expected {b}, got {a}");
        }
    }

    #[test]
    fn gain_smoothing_ramps_without_jumps() {
        let mut gain = Gain::new(0.0);
        gain.prepare(48_000.0, 1024);
        gain.set_db(-12.0);

        let mut buf = vec![1.0_f32; 1024];
        gain.process(&mut buf);

        // El primer valor debe estar cerca de 1.0 y el final cerca de -12 dB (~0.251).
        assert!(buf[0] > 0.99);
        let expected = db_to_lin(-12.0);
        assert!((buf[1023] - expected).abs() < 1e-3);
        // Transición monotónica decreciente.
        for w in buf.windows(2) {
            assert!(w[1] <= w[0] + 1e-7);
        }
    }

    #[test]
    fn overdrive_output_is_finite_and_bounded() {
        let mut od = Overdrive::new();
        od.set_drive_db(36.0);
        od.set_tone_hz(5_000.0);
        od.set_level_db(-6.0);
        od.prepare(48_000.0, 512);

        let level_lin = db_to_lin(-6.0);
        let limit = level_lin * 1.01;

        for block in 0..20 {
            let mut buf: Vec<f32> = (0..512)
                .map(|i| {
                    let t = (block * 512 + i) as f32 / 48_000.0;
                    (2.0 * std::f32::consts::PI * 220.0 * t).sin() * 0.9
                })
                .collect();
            od.process(&mut buf);
            for &y in &buf {
                assert!(y.is_finite(), "non-finite sample {y}");
                assert!(
                    y.abs() <= limit,
                    "sample {y} exceeded limit {limit} (level_lin={level_lin})"
                );
            }
        }
    }

    #[test]
    fn noise_gate_silences_low_noise_and_passes_loud_signal() {
        let mut gate = NoiseGate::new(-40.0);
        gate.prepare(48_000.0, 512);

        // 1. Ruido de bajo nivel (-60 dB = 0.001) durante 0.5 s -> debe quedar silenciado.
        let mut quiet = vec![0.0; 24_000];
        for (i, s) in quiet.iter_mut().enumerate() {
            *s = (i as f32 * 0.37).sin() * 0.001;
        }
        gate.process(&mut quiet);
        let max_quiet = quiet[12_000..]
            .iter()
            .fold(0.0_f32, |acc, &x| acc.max(x.abs()));
        assert!(max_quiet < 1e-5, "expected silence, got max {max_quiet}");

        // 2. Señal fuerte (-6 dB = 0.5) durante 0.25 s -> debe pasar casi intacta tras el ataque.
        let mut loud: Vec<f32> = (0..12_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let expected_tail: Vec<f32> = loud[6_000..].to_vec();
        gate.process(&mut loud);
        for (y, x) in loud[6_000..].iter().zip(expected_tail.iter()) {
            assert!((y - x).abs() < 1e-3, "expected {x}, got {y}");
        }

        // 3. Vuelve el ruido bajo durante 1 s -> debe volver a cerrarse.
        let mut back_to_quiet: Vec<f32> = (0..48_000)
            .map(|i| (i as f32 * 0.37).sin() * 0.001)
            .collect();
        gate.process(&mut back_to_quiet);
        let max_tail_quiet = back_to_quiet[40_000..]
            .iter()
            .fold(0.0_f32, |acc, &x| acc.max(x.abs()));
        assert!(
            max_tail_quiet < 1e-4,
            "expected gate to re-close, got {max_tail_quiet}"
        );
    }

    #[test]
    fn dc_blocker_removes_constant_offset() {
        let mut dc = DcBlocker::new(15.0);
        dc.prepare(48_000.0);

        let mut buf = vec![0.5_f32; 48_000];
        dc.process(&mut buf);

        let tail_max = buf[24_000..]
            .iter()
            .fold(0.0_f32, |acc, &x| acc.max(x.abs()));
        assert!(tail_max < 1e-4, "DC offset remained: {tail_max}");
    }

    #[test]
    fn no_nan_for_zeros_huge_and_nonfinite_inputs() {
        let mut chain = Chain::new();
        chain.push(NoiseGate::new(-50.0), false);
        chain.push(Overdrive::new(), false);
        chain.push(Gain::new(6.0), false);
        chain.prepare(48_000.0, 256);

        let cases = [
            0.0_f32,
            -0.0,
            1e10,
            -1e10,
            1e35,
            -1e35,
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            1e-40,
        ];

        let mut buf = vec![0.0_f32; 256];
        for (i, s) in buf.iter_mut().enumerate() {
            *s = cases[i % cases.len()];
        }
        chain.process(&mut buf);

        for (i, &y) in buf.iter().enumerate() {
            assert!(!y.is_nan(), "NaN at index {i}");
            assert!(y.is_finite(), "non-finite {y} at index {i}");
        }

        // Cadena en bypass con ceros sigue siendo ceros.
        chain.set_bypass(0, true);
        chain.set_bypass(1, true);
        chain.set_bypass(2, true);
        let mut zeros = vec![0.0_f32; 128];
        chain.process(&mut zeros);
        assert!(zeros.iter().all(|&x| x == 0.0));
    }
}
