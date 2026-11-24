use std::any::Any;

pub mod analyzer;
pub mod gain;
pub mod gate;
pub mod overdrive;
pub mod resampler;

pub use analyzer::{
    compute_spectrum_db, compute_waveform_metrics, fft_in_place, find_rising_zero_crossing,
    WaveformMetrics,
};
pub use gain::Gain;
pub use gate::NoiseGate;
pub use overdrive::Overdrive;
pub use resampler::Resampler;

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

/// Escala de mapeo entre un valor físico y el ángulo normalizado [0, 1] de la perilla.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamScale {
    Linear,
    Logarithmic,
}

/// Metadatos autodescriptivos de un parámetro de pedal para construir controles reactivos.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub name: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub scale: ParamScale,
    pub decimals: u8,
}

impl ParamSpec {
    #[inline]
    pub fn clamp(&self, val: f32) -> f32 {
        if val.is_finite() {
            val.clamp(self.min, self.max)
        } else {
            self.default
        }
    }

    /// Convierte el valor en unidades físicas a rango normalizado `0.0..=1.0`.
    #[inline]
    pub fn to_normalized(&self, val: f32) -> f32 {
        let v = self.clamp(val);
        if self.max <= self.min {
            return 0.0;
        }
        match self.scale {
            ParamScale::Linear => ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0),
            ParamScale::Logarithmic => {
                if self.min > 0.0 && self.max > self.min {
                    ((v / self.min).ln() / (self.max / self.min).ln()).clamp(0.0, 1.0)
                } else {
                    ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
                }
            }
        }
    }

    /// Convierte un valor normalizado `0.0..=1.0` a unidades físicas del parámetro.
    #[inline]
    pub fn from_normalized(&self, norm: f32) -> f32 {
        let n = if norm.is_finite() {
            norm.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let raw = match self.scale {
            ParamScale::Linear => self.min + n * (self.max - self.min),
            ParamScale::Logarithmic => {
                if self.min > 0.0 && self.max > self.min {
                    self.min * (self.max / self.min).powf(n)
                } else {
                    self.min + n * (self.max - self.min)
                }
            }
        };
        self.clamp(raw)
    }

    pub fn format_value(&self, val: f32) -> String {
        let v = self.clamp(val);
        if self.unit == "dB" && self.min <= -100.0 && v <= -100.0 {
            return "OFF".to_string();
        }
        let prec = self.decimals as usize;
        if self.unit.is_empty() {
            format!("{v:.prec$}")
        } else {
            format!("{v:.prec$} {}", self.unit)
        }
    }
}

/// Interfaz común para todos los efectos de audio en tiempo real.
///
/// Ninguna implementación debe reservar memoria, bloquear mutexes ni hacer E/S en `process`.
pub trait Effect: Send + AsAny {
    fn name(&self) -> &'static str;
    fn accent_color(&self) -> [u8; 3] {
        [230, 126, 34]
    }
    fn params(&self) -> &'static [ParamSpec];
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
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

/// Cadena ordenada de efectos con estado de bypass individual y orden dinámico en cascada.
pub struct Chain {
    slots: Vec<(Box<dyn Effect>, bool)>,
    order: Vec<usize>,
}

impl Default for Chain {
    fn default() -> Self {
        Self::new()
    }
}

impl Chain {
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            order: Vec::new(),
        }
    }

    /// Agrega un efecto al final de la cadena. `bypass = false` significa activo.
    pub fn push<E: Effect + 'static>(&mut self, effect: E, bypass: bool) -> usize {
        let idx = self.slots.len();
        self.slots.push((Box::new(effect), bypass));
        self.order.push(idx);
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

    pub fn slot(&self, index: usize) -> Option<&(Box<dyn Effect>, bool)> {
        self.slots.get(index)
    }

    pub fn slot_mut(&mut self, index: usize) -> Option<&mut (Box<dyn Effect>, bool)> {
        self.slots.get_mut(index)
    }

    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Sincroniza el orden de cascada sin reservar memoria.
    pub fn sync_order_from<F: Fn(usize) -> usize>(&mut self, get_order_at: F) {
        let count = self.slots.len().min(self.order.len());
        for i in 0..count {
            let idx = get_order_at(i);
            if idx < self.slots.len() {
                self.order[i] = idx;
            }
        }
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
    fn name(&self) -> &'static str {
        "Chain"
    }

    fn params(&self) -> &'static [ParamSpec] {
        &[]
    }

    fn get_param(&self, _index: usize) -> f32 {
        0.0
    }

    fn set_param(&mut self, _index: usize, _value: f32) {}

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        for (fx, _) in &mut self.slots {
            fx.prepare(sample_rate, max_block);
        }
    }

    fn process(&mut self, buf: &mut [f32]) {
        for i in 0..self.order.len() {
            let idx = self.order[i];
            if let Some((fx, bypass)) = self.slots.get_mut(idx) {
                if !*bypass {
                    fx.process(buf);
                }
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

    #[test]
    fn resampler_passthrough_is_exact() {
        let mut r = Resampler::new(48_000, 48_000);
        assert!(r.is_passthrough());
        let input: Vec<f32> = (0..128).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut out = Vec::with_capacity(128);
        for &x in &input {
            r.process_sample(x, |y| out.push(y));
        }
        assert_eq!(out, input);
    }

    #[test]
    fn resampler_upsample_44100_to_192000_count_and_bounded() {
        let mut r = Resampler::new(44_100, 192_000);
        assert!(!r.is_passthrough());
        let mut out = Vec::with_capacity(192_010);
        for i in 0..44_100 {
            let x = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44_100.0).sin();
            r.process_sample(x, |y| out.push(y));
        }
        let diff = (out.len() as isize - 192_000).abs();
        assert!(diff <= 2, "unexpected count {}, diff {}", out.len(), diff);
        for &y in &out {
            assert!(y.is_finite());
            assert!(y.abs() <= 1.02, "sample exceeded bound: {y}");
        }
    }

    #[test]
    fn resampler_downsample_96000_to_48000_count() {
        let mut r = Resampler::new(96_000, 48_000);
        let mut out = Vec::with_capacity(4805);
        for i in 0..9_600 {
            let x = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 96_000.0).sin();
            r.process_sample(x, |y| out.push(y));
        }
        let diff = (out.len() as isize - 4_800).abs();
        assert!(diff <= 2, "unexpected count {}, diff {}", out.len(), diff);
        for &y in &out {
            assert!(y.is_finite());
            assert!(y.abs() <= 1.02, "sample exceeded bound: {y}");
        }
    }

    #[test]
    fn metadata_params_and_chain_reorder_work() {
        let lin_spec = ParamSpec {
            name: "Drive",
            unit: "dB",
            min: 0.0,
            max: 40.0,
            default: 12.0,
            scale: ParamScale::Linear,
            decimals: 1,
        };
        let norm = lin_spec.to_normalized(20.0);
        assert!((norm - 0.5).abs() < 1e-6);
        assert!((lin_spec.from_normalized(norm) - 20.0).abs() < 1e-5);

        let log_spec = ParamSpec {
            name: "Tone",
            unit: "Hz",
            min: 500.0,
            max: 12_000.0,
            default: 4_000.0,
            scale: ParamScale::Logarithmic,
            decimals: 0,
        };
        let log_norm = log_spec.to_normalized(2_449.49);
        assert!((log_spec.from_normalized(log_norm) - 2_449.49).abs() < 0.5);

        let mut gate = NoiseGate::new(-60.0);
        assert_eq!(gate.name(), "Noise Gate");
        assert_eq!(gate.params().len(), 1);
        gate.set_param(0, -45.0);
        assert!((gate.get_param(0) - (-45.0)).abs() < 1e-6);

        let mut od = Overdrive::new();
        assert_eq!(od.name(), "Overdrive");
        assert_eq!(od.params().len(), 3);
        od.set_param(0, 24.0);
        od.set_param(1, 3_500.0);
        od.set_param(2, -3.0);
        assert!((od.get_param(0) - 24.0).abs() < 1e-6);
        assert!((od.get_param(1) - 3_500.0).abs() < 1e-3);
        assert!((od.get_param(2) - (-3.0)).abs() < 1e-6);

        let mut gain = Gain::new(-6.0);
        assert_eq!(gain.name(), "Master Volume");
        assert_eq!(gain.params().len(), 1);
        gain.set_param(0, -12.0);
        assert!((gain.get_param(0) - (-12.0)).abs() < 1e-6);

        // Verificar reordenamiento de Chain: Overdrive antes que Gain vs Gain antes que Overdrive.
        let mut chain = Chain::new();
        let mut od_slot = Overdrive::new();
        od_slot.set_drive_db(30.0);
        od_slot.set_level_db(0.0);
        chain.push(od_slot, false); // idx 0
        chain.push(Gain::new(-20.0), false); // idx 1
        chain.prepare(48_000.0, 64);

        let mut buf_a = vec![0.2_f32; 64];
        chain.process(&mut buf_a);

        chain.reset();
        chain.sync_order_from(|pos| if pos == 0 { 1 } else { 0 });
        let mut buf_b = vec![0.2_f32; 64];
        chain.process(&mut buf_b);

        let rms_a: f32 = buf_a.iter().map(|x| x * x).sum();
        let rms_b: f32 = buf_b.iter().map(|x| x * x).sum();
        // Al atenuar -20 dB después del overdrive vs antes del overdrive el nivel difiere claramente.
        assert!((rms_a - rms_b).abs() > 1e-4);
    }

    #[test]
    fn analyzer_fft_and_crest_factor_match_pure_sine() {
        const N: usize = 2048;
        let sr = 48_000.0_f32;
        let freq = 1_000.0_f32;
        let amp = 0.5_f32; // -6.02 dBFS

        let samples: Vec<f32> = (0..N)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / sr).sin())
            .collect();

        let metrics = compute_waveform_metrics(&samples);
        assert!(
            (metrics.crest_factor_db - 3.0103).abs() < 0.12,
            "unexpected crest factor {}",
            metrics.crest_factor_db
        );

        let mut spectrum = vec![-100.0_f32; N / 2];
        compute_spectrum_db(&samples, &mut spectrum);

        let (peak_bin, &peak_db) = spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let expected_bin = ((freq * N as f32) / sr).round() as isize;
        assert!(
            (peak_bin as isize - expected_bin).abs() <= 1,
            "expected bin near {expected_bin}, got {peak_bin}"
        );
        assert!(
            (peak_db - (-6.02)).abs() < 1.5,
            "expected peak near -6.02 dBFS, got {peak_db}"
        );

        let zc = find_rising_zero_crossing(&samples[10..], 200);
        assert!(zc > 0 && zc < 60);
    }
}
