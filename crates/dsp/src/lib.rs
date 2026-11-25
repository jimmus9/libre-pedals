use std::any::Any;

pub mod adaa;
pub mod analyzer;
pub mod cabinet_ir;
pub mod chorus;
pub mod clean_boost;
pub mod d5_experiment;
pub mod delay;
pub mod delay_line;
pub mod denormals;
pub mod gain;
pub mod gate;
pub mod limiter;
pub mod matched_eq;
pub mod opto_comp;
pub mod overdrive;
pub mod oversampler;
pub mod parametric_eq;
pub mod phaser;
pub mod resampler;
pub mod reverb;
pub mod smoother;
pub mod svf;
pub mod tape_limiter;
pub mod tremolo;
pub mod tube_preamp;
pub mod wah;
pub mod wav;

pub use adaa::{AsymSoftClipAdaa1, HalfSampleDelay, TanhAdaa1};
pub use analyzer::{
    compute_esr_db, compute_harmonic_metrics, compute_imd_db, compute_spectrum_db,
    compute_spectrum_db_with_window, compute_waveform_metrics, fft_in_place,
    find_rising_zero_crossing, generate_coherent_sine, HarmonicMetrics, WaveformMetrics,
    WindowKind,
};
pub use cabinet_ir::{
    partition_size_for_rate, CabinetIr, CustomIrAsset, PreparedIrPartitions, UpolsConvolver,
};
pub use chorus::Chorus;
pub use clean_boost::CleanBoost;
pub use delay::Delay;
pub use delay_line::{FractionalDelayLine, OnePoleAllPassTpt, SchroederAllPass};
pub use denormals::{flush_denormal, flush_denormal_f64, ScopedNoDenormals};
pub use gain::Gain;
pub use gate::NoiseGate;
pub use limiter::BrickwallLimiter;
pub use matched_eq::{MatchedBiquad, MatchedFilterKind};
pub use opto_comp::OptoCompressor;
pub use overdrive::Overdrive;
pub use oversampler::{adaptive_oversampling_factor, HalfBandStage2x, Oversampler};
pub use parametric_eq::ParametricEq;
pub use phaser::Phaser;
pub use resampler::Resampler;
pub use reverb::Reverb;
pub use smoother::{ModeCrossfader, OnePoleSmoother};
pub use svf::{SvfMode, SvfTpt};
pub use tape_limiter::SoftClipTape;
pub use tremolo::Tremolo;
pub use tube_preamp::TubePreamp;
pub use wah::Wah;
pub use wav::WavIrData;

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
    Stepped { labels: &'static [&'static str] },
}

/// Metadatos autodescriptivos de un parámetro de pedal para construir controles reactivos.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub scale: ParamScale,
    pub decimals: u8,
}

impl ParamSpec {
    /// Índice discreto acotado para parámetros [`ParamScale::Stepped`] (o entero redondeado).
    #[inline]
    pub fn step_index(&self, val: f32) -> usize {
        let v = self.clamp(val);
        match self.scale {
            ParamScale::Stepped { labels } => {
                if labels.is_empty() {
                    0
                } else {
                    (v.round() as isize).clamp(0, (labels.len() - 1) as isize) as usize
                }
            }
            ParamScale::Linear | ParamScale::Logarithmic => v.round().max(0.0) as usize,
        }
    }

    #[inline]
    pub fn clamp(&self, val: f32) -> f32 {
        let v = if val.is_finite() { val } else { self.default };
        match self.scale {
            ParamScale::Stepped { labels } => {
                if labels.is_empty() {
                    0.0
                } else {
                    v.round().clamp(0.0, (labels.len() - 1) as f32)
                }
            }
            ParamScale::Linear | ParamScale::Logarithmic => v.clamp(self.min, self.max),
        }
    }

    /// Convierte el valor en unidades físicas a rango normalizado `0.0..=1.0`.
    #[inline]
    pub fn to_normalized(&self, val: f32) -> f32 {
        let v = self.clamp(val);
        match self.scale {
            ParamScale::Stepped { labels } => {
                let last = labels.len().saturating_sub(1);
                if last == 0 {
                    0.0
                } else {
                    (v.round() / last as f32).clamp(0.0, 1.0)
                }
            }
            ParamScale::Linear => {
                if self.max <= self.min {
                    0.0
                } else {
                    ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
                }
            }
            ParamScale::Logarithmic => {
                if self.max <= self.min {
                    0.0
                } else if self.min > 0.0 {
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
            ParamScale::Stepped { labels } => {
                let last = labels.len().saturating_sub(1);
                (n * last as f32).round()
            }
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
        if let ParamScale::Stepped { labels } = self.scale {
            if labels.is_empty() {
                return String::new();
            }
            let idx = self.step_index(v);
            return labels[idx].to_string();
        }
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
    fn kind(&self) -> &'static str;
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
    /// Latencia algorítmica añadida por el efecto en muestras a la frecuencia actual de `prepare()`.
    fn latency_samples(&self) -> usize {
        0
    }
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
    fn kind(&self) -> &'static str {
        "chain"
    }

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

    fn latency_samples(&self) -> usize {
        self.order
            .iter()
            .filter_map(|&idx| self.slots.get(idx))
            .filter(|(_, bypass)| !*bypass)
            .map(|(fx, _)| fx.latency_samples())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_zero_db_is_identity() {
        let mut gain = Gain::new(0.0);
        gain.prepare(48_000.0, 256);

        let original: Vec<f32> = (0..256).map(|i| (i as f32 * 0.123).sin() * 0.75).collect();
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

        // 2. Señal fuerte (-6 dB = 0.5) durante 0.25 s -> debe pasar casi intacta tras el ataque
        // (compensando la latencia fija de 1 ms del lookahead).
        let latency = gate.latency_samples();
        let raw_loud: Vec<f32> = (0..12_000)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48_000.0).sin() * 0.5)
            .collect();
        let mut loud = raw_loud.clone();
        gate.process(&mut loud);
        for i in 6_000..12_000 {
            let y = loud[i];
            let x = raw_loud[i - latency];
            assert!((y - x).abs() < 1e-3, "expected {x}, got {y} at {i}");
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
        chain.push(Wah::new(), false);
        chain.push(OptoCompressor::new(), false);
        chain.push(Phaser::new(), false);
        chain.push(CleanBoost::new(), false);
        chain.push(Overdrive::new(), false);
        chain.push(TubePreamp::new(), false);
        chain.push(ParametricEq::new(), false);
        chain.push(CabinetIr::new(), false);
        chain.push(Chorus::new(), false);
        chain.push(Tremolo::new(), false);
        chain.push(Delay::new(), false);
        chain.push(Reverb::new(), false);
        chain.push(SoftClipTape::new(), false);
        chain.push(BrickwallLimiter::new(), false);
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
        for idx in 0..chain.len() {
            chain.set_bypass(idx, true);
        }
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
            id: "drive_db",
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
            id: "tone_hz",
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
        assert_eq!(gate.params().len(), 5);
        gate.set_param(0, -45.0);
        assert!((gate.get_param(0) - (-45.0)).abs() < 1e-6);

        let mut od = Overdrive::new();
        assert_eq!(od.name(), "Overdrive");
        assert_eq!(od.params().len(), 6);
        od.set_param(0, 24.0);
        od.set_param(1, 720.0);
        od.set_param(2, 65.0);
        od.set_param(3, 25.0);
        od.set_param(4, 3_500.0);
        od.set_param(5, -3.0);
        assert!((od.get_param(0) - 24.0).abs() < 1e-6);
        assert!((od.get_param(1) - 720.0).abs() < 1e-2);
        assert!((od.get_param(2) - 65.0).abs() < 1e-6);
        assert!((od.get_param(3) - 25.0).abs() < 1e-6);
        assert!((od.get_param(4) - 3_500.0).abs() < 1e-3);
        assert!((od.get_param(5) - (-3.0)).abs() < 1e-6);

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

    #[test]
    fn hiir_halfband_passband_stopband_and_aliasing_reduction() {
        assert_eq!(adaptive_oversampling_factor(32_000.0), 8);
        assert_eq!(adaptive_oversampling_factor(44_100.0), 4);
        assert_eq!(adaptive_oversampling_factor(48_000.0), 4);
        assert_eq!(adaptive_oversampling_factor(96_000.0), 2);
        assert_eq!(adaptive_oversampling_factor(128_000.0), 2);
        assert_eq!(adaptive_oversampling_factor(192_000.0), 1);

        let stage = HalfBandStage2x::from_spec(8, 0.045);
        // Banda de paso (0.10 * 96 kHz = 9.6 kHz) plana < 0.01 dB.
        let pb_db = lin_to_db(stage.magnitude_response_at(0.10) as f32);
        assert!(pb_db.abs() < 0.01, "passband error too high: {pb_db} dB");
        // Banda de rechazo (> 0.28 -> ~27 kHz @ 96 kHz) > 85 dB.
        let sb_db = lin_to_db(stage.magnitude_response_at(0.32) as f32);
        assert!(sb_db < -85.0, "stopband rejection insufficient: {sb_db} dB");

        // Prueba de aliasing: tono agudo 4,370 Hz con +24 dB de drive sobre tanh(x) @ 48 kHz.
        const N: usize = 4096;
        const WARMUP: usize = 1024;
        let sr = 48_000.0_f32;
        let (input_full, freq_exact) = generate_coherent_sine(4_370.0, sr, N + WARMUP, 1.0);
        let drive = db_to_lin(24.0);

        // Referencia sin antialiasing (1x tanh directo).
        let naive_full: Vec<f32> = input_full
            .iter()
            .map(|&x| (x as f64 * drive as f64).tanh() as f32)
            .collect();
        let naive_m = compute_harmonic_metrics(
            &naive_full[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(freq_exact),
        );

        // HIIR 4x + TanhAdaa1.
        let mut os = Oversampler::new(sr, 256);
        assert_eq!(os.factor(), 4);
        let mut adaa = TanhAdaa1::default();
        let mut clean_full = input_full.clone();
        for chunk in clean_full.chunks_mut(256) {
            os.process_block(chunk, |buf, _| {
                for s in buf.iter_mut() {
                    *s = adaa.process_sample(*s * drive as f64);
                }
            });
        }
        let clean_m = compute_harmonic_metrics(
            &clean_full[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(freq_exact),
        );

        let improvement_db = clean_m.aliasing_snr_db - naive_m.aliasing_snr_db;
        assert!(
            improvement_db > 25.0,
            "expected > 25 dB aliasing improvement, got {improvement_db:.2} dB (naive ASNR={:.2}, clean ASNR={:.2})",
            naive_m.aliasing_snr_db,
            clean_m.aliasing_snr_db
        );
    }

    #[test]
    fn adaa1_tanh_and_asym_finite_at_extremes_and_even_harmonics() {
        let mut tanh_adaa = TanhAdaa1::default();
        for &x in &[
            0.0_f64, 1e-12, 1e-9, -1e-9, 50.0, -50.0, 1e6, -1e6, 0.25, 0.25,
        ] {
            let y = tanh_adaa.process_sample(x);
            assert!(
                y.is_finite() && y.abs() <= 1.0001,
                "bad ADAA1 out {y} for {x}"
            );
        }

        const N: usize = 4096;
        let sr = 48_000.0_f32;
        let (sine, f0) = generate_coherent_sine(1_000.0, sr, N + 512, 1.0);

        let mut sym = AsymSoftClipAdaa1::new(1.0, 1.0);
        let sym_out: Vec<f32> = sine
            .iter()
            .map(|&x| sym.process_sample(x as f64 * 4.0) as f32)
            .collect();
        let sym_m =
            compute_harmonic_metrics(&sym_out[512..], sr, WindowKind::BlackmanHarris4, Some(f0));
        assert!(
            sym_m.even_vs_fund_db < -60.0,
            "symmetric ADAA1 leaked even harmonics: {} dB",
            sym_m.even_vs_fund_db
        );

        let mut asym = AsymSoftClipAdaa1::new(1.8, 0.65);
        let asym_out: Vec<f32> = sine
            .iter()
            .map(|&x| asym.process_sample(x as f64 * 4.0) as f32)
            .collect();
        let asym_m =
            compute_harmonic_metrics(&asym_out[512..], sr, WindowKind::BlackmanHarris4, Some(f0));
        assert!(
            asym_m.even_vs_fund_db > -25.0,
            "asymmetric ADAA1 should yield clear even harmonics: {} dB",
            asym_m.even_vs_fund_db
        );
    }

    #[test]
    fn svf_tpt_rapid_modulation_remains_stable_and_bounded() {
        let sr = 48_000.0_f32;
        let mut svf = SvfTpt::new(sr);
        let mut max_abs = 0.0_f32;
        for i in 0..8192 {
            let phase = (i as f32) * 0.031;
            let fc = 100.0 + 11_900.0 * (0.5 + 0.5 * phase.sin());
            svf.set_params(SvfMode::BandPass, fc, 8.0, 0.0);
            let x = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr).sin() * 0.8;
            let y = svf.process_sample(x);
            assert!(y.is_finite(), "SVF exploded at sample {i}");
            max_abs = max_abs.max(y.abs());
        }
        assert!(
            max_abs > 0.05 && max_abs < 5.0,
            "unexpected modulated peak {max_abs}"
        );

        svf.reset();
        svf.set_params(SvfMode::Bell, 1_500.0, 1.5, 6.0);
        assert!((svf.magnitude_db_at(1_500.0) - 6.0).abs() < 0.05);
        assert!(svf.magnitude_db_at(50.0).abs() < 0.05);
    }

    #[test]
    fn matched_biquad_no_nyquist_cramping_vs_high_fs() {
        let mut eq_48 = MatchedBiquad::new(48_000.0);
        let mut eq_192 = MatchedBiquad::new(192_000.0);

        eq_48.set_params(MatchedFilterKind::HighShelf, 10_000.0, 0.707, 6.0);
        eq_192.set_params(MatchedFilterKind::HighShelf, 10_000.0, 0.707, 6.0);
        assert!((eq_48.magnitude_db_at(10_000.0) - 3.0).abs() < 0.15);
        assert!(
            (eq_48.magnitude_db_at(20_000.0) - eq_192.magnitude_db_at(20_000.0)).abs() < 0.5,
            "HighShelf cramped at 20 kHz: 48k={:.2} dB vs 192k={:.2} dB",
            eq_48.magnitude_db_at(20_000.0),
            eq_192.magnitude_db_at(20_000.0)
        );

        eq_48.set_params(MatchedFilterKind::Peaking, 14_000.0, 1.5, 9.0);
        eq_192.set_params(MatchedFilterKind::Peaking, 14_000.0, 1.5, 9.0);
        assert!((eq_48.magnitude_db_at(14_000.0) - 9.0).abs() < 0.1);
        assert!(
            (eq_48.magnitude_db_at(20_000.0) - eq_192.magnitude_db_at(20_000.0)).abs() < 0.6,
            "Peaking cramped near Nyquist: 48k={:.2} dB vs 192k={:.2} dB",
            eq_48.magnitude_db_at(20_000.0),
            eq_192.magnitude_db_at(20_000.0)
        );

        // Corte exacto espejo (-9 dB vs +9 dB).
        let mut eq_cut = MatchedBiquad::new(48_000.0);
        eq_cut.set_params(MatchedFilterKind::Peaking, 14_000.0, 1.5, -9.0);
        assert!((eq_48.magnitude_db_at(14_000.0) + eq_cut.magnitude_db_at(14_000.0)).abs() < 0.05);
    }

    #[test]
    fn param_spec_stepped_mapping_and_mode_crossfader_smooth() {
        let spec = ParamSpec {
            id: "topology",
            name: "Circuit",
            unit: "",
            min: 0.0,
            max: 1.0,
            default: 0.0,
            scale: ParamScale::Stepped {
                labels: &["Op-Amp", "JFET"],
            },
            decimals: 0,
        };
        assert_eq!(spec.step_index(0.2), 0);
        assert_eq!(spec.step_index(0.8), 1);
        assert_eq!(spec.step_index(99.0), 1);
        assert_eq!(spec.format_value(0.0), "Op-Amp");
        assert_eq!(spec.format_value(1.0), "JFET");
        assert!((spec.to_normalized(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(spec.from_normalized(0.6), 1.0);

        let mut cf = ModeCrossfader::new(5.0, 48_000.0, 0);
        assert!(!cf.is_crossfading());
        cf.set_mode(1);
        assert!(cf.is_crossfading());
        assert_eq!(cf.pair(), (0, 1));

        let mut prev_out = 1.0_f32;
        let mut max_delta = 0.0_f32;
        for _ in 0..300 {
            let (g0, g1) = cf.advance_gains();
            assert!((g0 + g1 - 1.0).abs() < 1e-5);
            let out = g0 - g1;
            max_delta = max_delta.max((out - prev_out).abs());
            prev_out = out;
        }
        assert!(!cf.is_crossfading());
        assert_eq!(cf.active_mode(), 1);
        assert!(max_delta < 0.05, "crossfade jump too large: {max_delta}");
    }

    #[test]
    fn offline_analyzer_metrics_coherent_thd_imd_and_esr() {
        const N: usize = 4096;
        let sr = 48_000.0_f32;
        let (pure, f0) = generate_coherent_sine(1_000.0, sr, N, 0.5);
        let m_pure = compute_harmonic_metrics(&pure, sr, WindowKind::BlackmanHarris4, Some(f0));
        assert!(
            m_pure.thd_db < -85.0,
            "BH4 pure sine THD floor too high: {} dB",
            m_pure.thd_db
        );
        assert!(
            m_pure.aliasing_snr_db > 85.0,
            "BH4 pure sine ASNR too low: {} dB",
            m_pure.aliasing_snr_db
        );

        // IMD dos tonos: lineal vs no lineal.
        let f1 = 220.0_f32;
        let f2 = 330.0_f32;
        let two_tone: Vec<f32> = (0..N)
            .map(|i| {
                let t = i as f32 / sr;
                0.35 * (2.0 * std::f32::consts::PI * f1 * t).sin()
                    + 0.35 * (2.0 * std::f32::consts::PI * f2 * t).sin()
            })
            .collect();
        let imd_lin = compute_imd_db(&two_tone, sr, f1, f2);
        let nonlin: Vec<f32> = two_tone.iter().map(|&x| (x * 4.0).tanh()).collect();
        let imd_clip = compute_imd_db(&nonlin, sr, f1, f2);
        assert!(
            imd_lin < -65.0,
            "linear two-tone IMD unexpected: {imd_lin} dB"
        );
        assert!(imd_clip > -35.0, "clipped IMD too low: {imd_clip} dB");

        // ESR: idénticos <= -140 dB, diferencia conocida coherente.
        assert!(compute_esr_db(&pure, &pure) <= -139.0);
        let shifted: Vec<f32> = pure.iter().map(|&x| x * 0.9).collect();
        let esr = compute_esr_db(&pure, &shifted);
        assert!(
            (esr - (-20.0)).abs() < 0.05,
            "expected -20 dB ESR, got {esr}"
        );
    }

    #[test]
    fn clean_boost_opamp_thd_subsonic_headroom_and_jfet_h2() {
        const WARMUP: usize = 2048;
        const N: usize = 4096;
        let sr = 48_000.0_f32;

        // 1) Op-Amp lineal: THD < -80 dB y caída subsónica en Mi grave (82.41 Hz) < 0.1 dB.
        let mut boost = CleanBoost::new();
        boost.prepare(sr, 256);
        boost.set_gain_db(6.0);
        boost.set_mode(0.0);
        boost.set_tilt_db(0.0);

        let (sine_1k, f_1k) = generate_coherent_sine(1_000.0, sr, WARMUP + N, 0.25);
        let mut out_1k = sine_1k.clone();
        for chunk in out_1k.chunks_mut(256) {
            boost.process(chunk);
        }
        let m_op = compute_harmonic_metrics(
            &out_1k[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_1k),
        );
        assert!(
            m_op.thd_db < -80.0,
            "Op-Amp boost THD too high: {} dB",
            m_op.thd_db
        );

        // Mi grave 82.41 Hz vs 1 kHz con ganancia 0 dB -> caída < 0.1 dB.
        boost.reset();
        boost.set_gain_db(0.0);
        let (low_e, _) = generate_coherent_sine(82.41, sr, WARMUP + N, 0.2);
        let (mid_ref, _) = generate_coherent_sine(1_000.0, sr, WARMUP + N, 0.2);
        let mut out_low = low_e.clone();
        let mut out_mid = mid_ref.clone();
        for chunk in out_low.chunks_mut(256) {
            boost.process(chunk);
        }
        boost.reset();
        for chunk in out_mid.chunks_mut(256) {
            boost.process(chunk);
        }
        let rms_low = compute_waveform_metrics(&out_low[WARMUP..]).rms_db;
        let rms_mid = compute_waveform_metrics(&out_mid[WARMUP..]).rms_db;
        assert!(
            (rms_mid - rms_low).abs() < 0.1,
            "30 Hz subsonic HP attenuated 82.41 Hz too much: diff = {:.3} dB",
            rms_mid - rms_low
        );

        // 2) Riel 18 V acotado frente a señal extrema (+26 dB sobre 0 dBFS).
        boost.reset();
        boost.set_gain_db(26.0);
        let mut hot = sine_1k.iter().map(|&x| x * 4.0).collect::<Vec<_>>();
        for chunk in hot.chunks_mut(256) {
            boost.process(chunk);
        }
        let mut max_hot = 0.0_f32;
        for &y in &hot {
            assert!(y.is_finite() && y.abs() <= 2.55, "18V rail exceeded: {y}");
            max_hot = max_hot.max(y.abs());
        }
        assert!(
            max_hot > 1.8,
            "expected 18V headroom utilization, got max={max_hot}"
        );

        // 3) JFET introduce H2 musical significativo respecto a Op-Amp.
        boost.reset();
        boost.set_mode(1.0);
        boost.set_gain_db(18.0);
        let (sig_j, f_j) = generate_coherent_sine(1_000.0, sr, WARMUP + N, 0.15);
        let mut out_j = sig_j;
        for chunk in out_j.chunks_mut(256) {
            boost.process(chunk);
        }
        let m_j =
            compute_harmonic_metrics(&out_j[WARMUP..], sr, WindowKind::BlackmanHarris4, Some(f_j));
        assert!(
            m_j.h2_db > -55.0,
            "JFET mode should generate warm H2: {} dB",
            m_j.h2_db
        );

        // 4) Tilt EQ balance (-6 dB vs +6 dB en graves y agudos).
        boost.reset();
        boost.set_mode(0.0);
        boost.set_gain_db(0.0);
        boost.set_tilt_db(6.0);
        let (bass, _) = generate_coherent_sine(120.0, sr, WARMUP + N, 0.2);
        let (treble, _) = generate_coherent_sine(5_000.0, sr, WARMUP + N, 0.2);
        let mut out_b = bass;
        let mut out_t = treble;
        for chunk in out_b.chunks_mut(256) {
            boost.process(chunk);
        }
        boost.reset();
        for chunk in out_t.chunks_mut(256) {
            boost.process(chunk);
        }
        let b_db = compute_waveform_metrics(&out_b[WARMUP..]).rms_db;
        let t_db = compute_waveform_metrics(&out_t[WARMUP..]).rms_db;
        assert!(
            t_db - b_db > 8.0,
            "Tilt +6 dB should boost highs vs lows: treble={t_db}, bass={b_db}"
        );
    }

    #[test]
    fn overdrive_unified_6knob_precut_asym_clean_blend_and_aliasing() {
        const WARMUP: usize = 2048;
        const N: usize = 4096;
        let sr = 48_000.0_f32;

        // 1) Pre-Cut 720 Hz reduce IMD en quintas graves respecto a Pre-Cut 35 Hz.
        let mut od = Overdrive::new();
        od.prepare(sr, 256);
        od.set_drive_db(28.0);
        od.set_asymmetry(0.0);
        od.set_clean_blend(0.0);
        od.set_tone_hz(10_000.0);
        od.set_level_db(0.0);

        let f1 = 110.0_f32;
        let f2 = 165.0_f32;
        let two_tone: Vec<f32> = (0..(WARMUP + N))
            .map(|i| {
                let t = i as f32 / sr;
                0.25 * (2.0 * std::f32::consts::PI * f1 * t).sin()
                    + 0.25 * (2.0 * std::f32::consts::PI * f2 * t).sin()
            })
            .collect();

        od.set_low_cut_hz(35.0);
        od.reset();
        let mut y_full = two_tone.clone();
        for chunk in y_full.chunks_mut(256) {
            od.process(chunk);
        }
        let imd_full = compute_imd_db(&y_full[WARMUP..], sr, f1, f2);

        od.set_low_cut_hz(720.0);
        od.reset();
        let mut y_ts = two_tone;
        for chunk in y_ts.chunks_mut(256) {
            od.process(chunk);
        }
        let imd_ts = compute_imd_db(&y_ts[WARMUP..], sr, f1, f2);
        assert!(
            imd_ts < imd_full - 6.0,
            "TS Pre-Cut 720 Hz should reduce bass IMD vs 35 Hz: ts={imd_ts} dB, full={imd_full} dB"
        );

        // 2) Asymmetry 0% (solo impares) vs 100% (pares marcados).
        let (sine_1k, f_1k) = generate_coherent_sine(1_000.0, sr, WARMUP + N, 0.25);
        od.set_low_cut_hz(60.0);
        od.set_drive_db(24.0);
        od.set_asymmetry(0.0);
        od.reset();
        let mut y_sym = sine_1k.clone();
        for chunk in y_sym.chunks_mut(256) {
            od.process(chunk);
        }
        let m_sym = compute_harmonic_metrics(
            &y_sym[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_1k),
        );
        assert!(
            m_sym.even_vs_fund_db < -55.0,
            "Asym 0% leaked even harmonics: {} dB",
            m_sym.even_vs_fund_db
        );

        od.set_asymmetry(100.0);
        od.reset();
        let mut y_asym = sine_1k;
        for chunk in y_asym.chunks_mut(256) {
            od.process(chunk);
        }
        let m_asym = compute_harmonic_metrics(
            &y_asym[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_1k),
        );
        assert!(
            m_asym.even_vs_fund_db > -26.0,
            "Asym 100% should yield strong even harmonics: {} dB",
            m_asym.even_vs_fund_db
        );

        // 3) Clean Blend: 100% plano en banda (incluso con Tone oscuro) y 50% sin muesca de comb filtering.
        od.set_drive_db(0.0);
        od.set_low_cut_hz(30.0);
        od.set_asymmetry(0.0);
        od.set_tone_hz(1_500.0);
        od.set_clean_blend(100.0);
        od.set_level_db(0.0);
        od.reset();
        let in_db = lin_to_db(0.1 / std::f32::consts::SQRT_2);
        for &freq_blend in &[1_000.0_f32, 8_000.0_f32] {
            let (tone_sig, _) = generate_coherent_sine(freq_blend, sr, WARMUP + N, 0.1);
            let mut y_100 = tone_sig;
            od.reset();
            for chunk in y_100.chunks_mut(256) {
                od.process(chunk);
            }
            let out_100_db = compute_waveform_metrics(&y_100[WARMUP..]).rms_db;
            assert!(
                (out_100_db - in_db).abs() < 0.25,
                "Clean Blend 100% no fue plano en {freq_blend} Hz: in={in_db:.2} dB, out={out_100_db:.2} dB"
            );
        }

        od.set_tone_hz(12_000.0);
        od.set_clean_blend(50.0);
        let (mid_tone, _) = generate_coherent_sine(4_000.0, sr, WARMUP + N, 0.1);
        let mut y_4k = mid_tone;
        od.reset();
        for chunk in y_4k.chunks_mut(256) {
            od.process(chunk);
        }
        let blend_4k_db = compute_waveform_metrics(&y_4k[WARMUP..]).rms_db;
        assert!(
            (blend_4k_db - in_db).abs() < 1.2,
            "Clean Blend 50% comb-filtered 4 kHz: in={in_db:.2} dB, out={blend_4k_db:.2} dB"
        );

        let (hi_tone, _) = generate_coherent_sine(8_000.0, sr, WARMUP + N, 0.1);
        let mut y_blend = hi_tone;
        od.reset();
        for chunk in y_blend.chunks_mut(256) {
            od.process(chunk);
        }
        let blend_8k_db = compute_waveform_metrics(&y_blend[WARMUP..]).rms_db;
        assert!(
            blend_8k_db > in_db - 2.8,
            "Clean Blend 50% cayó demasiado en 8 kHz (posible comb): in={in_db:.2} dB, out={blend_8k_db:.2} dB"
        );

        // 4) Rechazo de aliasing > 25 dB en 4,370 Hz (+24 dB drive @ 48 kHz) frente a tanh 1x.
        let (hi_stress, f_stress) = generate_coherent_sine(4_370.0, sr, WARMUP + N, 1.0);
        let naive_drive = db_to_lin(24.0);
        let naive_out: Vec<f32> = hi_stress
            .iter()
            .map(|&x| (x * naive_drive).tanh())
            .collect();
        let m_naive = compute_harmonic_metrics(
            &naive_out[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_stress),
        );

        od.set_drive_db(24.0);
        od.set_low_cut_hz(100.0);
        od.set_asymmetry(0.0);
        od.set_clean_blend(0.0);
        od.set_tone_hz(12_000.0);
        od.set_level_db(0.0);
        od.reset();
        let mut od_out = hi_stress;
        for chunk in od_out.chunks_mut(256) {
            od.process(chunk);
        }
        let m_od = compute_harmonic_metrics(
            &od_out[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_stress),
        );
        let gain_asnr = m_od.aliasing_snr_db - m_naive.aliasing_snr_db;
        assert!(
            gain_asnr > 25.0,
            "Overdrive aliasing reduction < 25 dB: +{gain_asnr:.2} dB (naive={}, od={})",
            m_naive.aliasing_snr_db,
            m_od.aliasing_snr_db
        );
    }

    #[test]
    fn tube_preamp_12ax7_koren_harmonics_dynamic_sag_and_fs_invariance() {
        const WARMUP: usize = 2048;
        const N: usize = 4096;

        // 1) Armónicos H2 + H3 y ausencia de offset DC.
        let sr = 48_000.0_f32;
        let mut tube = TubePreamp::new();
        tube.prepare(sr, 256);
        tube.set_gain_db(22.0);
        tube.set_stages(0.0);
        tube.set_bias_pct(50.0);
        tube.set_tone_db(0.0);
        tube.set_level_db(0.0);

        let (sine_1k, f_1k) = generate_coherent_sine(1_000.0, sr, WARMUP + N, 0.25);
        let mut y = sine_1k;
        for chunk in y.chunks_mut(256) {
            tube.process(chunk);
        }
        let mean = y[WARMUP..].iter().sum::<f32>() / N as f32;
        assert!(mean.abs() < 0.01, "12AX7 leaked DC mean={mean}");

        let m = compute_harmonic_metrics(&y[WARMUP..], sr, WindowKind::BlackmanHarris4, Some(f_1k));
        assert!(
            m.h2_db > -45.0,
            "12AX7 lacks even harmonics H2: {}",
            m.h2_db
        );
        assert!(m.h3_db > -45.0, "12AX7 lacks odd harmonics H3: {}", m.h3_db);

        // 2) Compresión dinámica por Bias/Sag ante ráfaga fuerte vs débil.
        tube.set_bias_pct(100.0);
        tube.reset();
        let (burst, _) = generate_coherent_sine(220.0, sr, 9600, 0.7);
        let mut y_sag = burst;
        for chunk in y_sag.chunks_mut(256) {
            tube.process(chunk);
        }
        let head_rms = compute_waveform_metrics(&y_sag[256..1024]).rms;
        let tail_rms = compute_waveform_metrics(&y_sag[6000..8500]).rms;
        assert!(
            tail_rms <= head_rms * 0.99,
            "dynamic sag did not compress sustained signal: head={head_rms}, tail={tail_rms}"
        );

        // 3) Conmutación suave 2x <-> 3x sin desbordamientos.
        tube.set_stages(1.0);
        let (sine_3x, _) = generate_coherent_sine(440.0, sr, 2048, 0.25);
        let mut y_3x = sine_3x;
        for chunk in y_3x.chunks_mut(128) {
            tube.process(chunk);
        }
        for &s in &y_3x {
            assert!(s.is_finite() && s.abs() < 5.0);
        }

        // 4) Invarianza de THD entre 44.1, 48, 96 y 192 kHz (< 1.5% absoluto con M(fs)).
        let mut thds = Vec::new();
        for &test_sr in &[44_100.0_f32, 48_000.0, 96_000.0, 192_000.0] {
            let mut tp = TubePreamp::new();
            let block = 256;
            let len = if test_sr > 100_000.0 { 16_384 } else { 4096 };
            let warm = if test_sr > 100_000.0 { 4096 } else { 2048 };
            tp.prepare(test_sr, block);
            tp.set_gain_db(18.0);
            tp.set_stages(0.0);
            tp.set_bias_pct(25.0);
            tp.set_tone_db(0.0);
            tp.set_level_db(0.0);
            let (s_in, f_ex) = generate_coherent_sine(1_000.0, test_sr, warm + len, 0.2);
            let mut s_out = s_in;
            for ch in s_out.chunks_mut(block) {
                tp.process(ch);
            }
            let hm = compute_harmonic_metrics(
                &s_out[warm..],
                test_sr,
                WindowKind::BlackmanHarris4,
                Some(f_ex),
            );
            thds.push(hm.thd_percent);
        }
        let max_t = thds.iter().copied().fold(0.0_f32, f32::max);
        let min_t = thds.iter().copied().fold(f32::INFINITY, f32::min);
        assert!(
            max_t - min_t < 1.5,
            "12AX7 THD drifted across sample rates: {thds:?}"
        );
    }

    #[test]
    fn noise_gate_advanced_hysteresis_sidechain_range_lookahead() {
        let sr = 48_000.0_f32;
        let t_open = -30.0_f32;

        // 1) Histéresis vs ausencia de histéresis cerca del umbral.
        let run_chatter = |hyst_db: f32| -> usize {
            let mut g = NoiseGate::new(t_open);
            g.prepare(sr, 64);
            g.set_hysteresis_db(hyst_db);
            g.set_hold_ms(0.0);
            g.set_release_ms(50.0);

            let mut transitions = 0usize;
            let mut prev_open = g.is_open();

            // 150 ms por encima del umbral para abrir.
            let open_samples = (0.150 * sr) as usize;
            for i in 0..open_samples {
                let s =
                    db_to_lin(-20.0) * (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / sr).sin();
                let mut sample = [s];
                g.process(&mut sample);
                if g.is_open() != prev_open {
                    transitions += 1;
                    prev_open = g.is_open();
                }
            }

            // 1.2 s oscilando 3 Hz alrededor de T - 1 dB (-34..-28 dBFS).
            let mod_samples = (1.200 * sr) as usize;
            for i in 0..mod_samples {
                let t = i as f32 / sr;
                let amp_db = t_open - 1.0 + 3.0 * (2.0 * std::f32::consts::PI * 3.0 * t).sin();
                let s = db_to_lin(amp_db) * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin();
                let mut sample = [s];
                g.process(&mut sample);
                if g.is_open() != prev_open {
                    transitions += 1;
                    prev_open = g.is_open();
                }
            }
            transitions
        };

        let trans_hyst = run_chatter(6.0);
        let trans_no_hyst = run_chatter(0.0);
        assert_eq!(
            trans_hyst, 1,
            "hysteresis failed to stop chatter: {trans_hyst}"
        );
        assert!(
            trans_no_hyst >= 4,
            "expected chatter without hysteresis, got {trans_no_hyst}"
        );

        // 2) Sidechain interno (120 Hz HP 4º orden): rechaza zumbido 60 Hz @ -30 dBFS (T=-40).
        let mut g_sc = NoiseGate::new(-40.0);
        g_sc.prepare(sr, 256);
        let mut hum: Vec<f32> = (0..48_000)
            .map(|i| db_to_lin(-30.0) * (2.0 * std::f32::consts::PI * 60.0 * i as f32 / sr).sin())
            .collect();
        for ch in hum.chunks_mut(256) {
            g_sc.process(ch);
        }
        let max_hum = hum[24_000..].iter().fold(0.0_f32, |m, &x| m.max(x.abs()));
        assert!(max_hum < 1e-5, "60 Hz hum opened gate: max={max_hum}");

        g_sc.reset();
        let tone_1k: Vec<f32> = (0..24_000)
            .map(|i| {
                db_to_lin(-30.0) * (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / sr).sin()
            })
            .collect();
        let mut out_1k = tone_1k.clone();
        for ch in out_1k.chunks_mut(256) {
            g_sc.process(ch);
        }
        let rms_in = compute_waveform_metrics(&tone_1k[12_000..]).rms_db;
        let rms_out = compute_waveform_metrics(&out_1k[12_000..]).rms_db;
        assert!(
            (rms_out - rms_in).abs() < 0.2,
            "1 kHz signal attenuated by gate: diff={}",
            rms_out - rms_in
        );

        // 3) Range (-20 dB): señal por debajo del umbral se atenúa exactamente -20 ± 0.5 dB.
        let mut g_rng = NoiseGate::new(-30.0);
        g_rng.prepare(sr, 256);
        g_rng.set_range_db(-20.0);
        let quiet_sig: Vec<f32> = (0..48_000)
            .map(|i| {
                db_to_lin(-50.0) * (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / sr).sin()
            })
            .collect();
        let mut quiet_out = quiet_sig.clone();
        for ch in quiet_out.chunks_mut(256) {
            g_rng.process(ch);
        }
        let rng_diff = compute_waveform_metrics(&quiet_out[36_000..]).rms_db
            - compute_waveform_metrics(&quiet_sig[36_000..]).rms_db;
        assert!(
            (rng_diff - (-20.0)).abs() < 0.5,
            "gate range attenuation unexpected: {rng_diff} dB"
        );

        // 4) Lookahead fijo 1 ms: latencia exacta y preservación del ataque inicial.
        let mut g_la_192 = NoiseGate::new(-40.0);
        g_la_192.prepare(192_000.0, 256);
        assert_eq!(g_la_192.latency_samples(), 192);

        let mut g_la = NoiseGate::new(-40.0);
        g_la.prepare(sr, 128);
        let lat = g_la.latency_samples();
        assert_eq!(lat, 48);

        let onset = 2_400usize;
        let mut burst_in = vec![0.0_f32; 4_800];
        for (i, slot) in burst_in.iter_mut().enumerate().skip(onset) {
            *slot = db_to_lin(-10.0)
                * (2.0 * std::f32::consts::PI * 1_000.0 * (i - onset) as f32 / sr).sin();
        }
        let mut burst_out = burst_in.clone();
        for ch in burst_out.chunks_mut(128) {
            g_la.process(ch);
        }
        let e_in: f64 = burst_in[onset..onset + 96]
            .iter()
            .map(|&x| (x as f64) * (x as f64))
            .sum();
        let e_out: f64 = burst_out[onset + lat..onset + lat + 96]
            .iter()
            .map(|&x| (x as f64) * (x as f64))
            .sum();
        assert!(
            e_out / e_in > 0.75,
            "lookahead clipped initial transient: ratio = {:.3}",
            e_out / e_in
        );
    }

    #[test]
    fn opto_comp_two_stage_release_program_dependence_and_low_thd() {
        let sr = 48_000.0_f32;
        let block = 48usize; // 1 ms por bloque

        let run_burst_recovery = |burst_ms: usize| -> (f32, f32, f32) {
            let mut comp = OptoCompressor::new();
            comp.prepare(sr, block);
            comp.set_peak_reduction(80.0);
            comp.set_mode(0.0); // Compress
            comp.set_mix(100.0);
            comp.set_gain_db(0.0);

            let mut phase = 0usize;
            for _ in 0..burst_ms {
                let mut b = [0.0_f32; 48];
                for s in &mut b {
                    *s = 0.5 * (2.0 * std::f32::consts::PI * 1_000.0 * phase as f32 / sr).sin();
                    phase += 1;
                }
                comp.process(&mut b);
            }
            let g0 = comp.gain_reduction_db();

            let mut gr_150 = 0.0_f32;
            let mut gr_1000 = 0.0_f32;
            for ms in 1..=1000 {
                let mut sil = [0.0_f32; 48];
                comp.process(&mut sil);
                if ms == 150 {
                    gr_150 = comp.gain_reduction_db();
                } else if ms == 1000 {
                    gr_1000 = comp.gain_reduction_db();
                }
            }
            (g0, gr_150, gr_1000)
        };

        let (g0_short, gr150_short, gr1000_short) = run_burst_recovery(300);
        assert!(g0_short > 6.0, "expected G0 > 6 dB, got {g0_short}");
        let recov_150 = 1.0 - gr150_short / g0_short;
        assert!(
            (0.35..=0.75).contains(&recov_150),
            "initial recovery at +150 ms outside [0.35, 0.75]: {recov_150}"
        );

        let (g0_long, _, gr1000_long) = run_burst_recovery(4000);
        let res_short = gr1000_short / g0_short;
        let res_long = gr1000_long / g0_long;
        assert!(
            res_long > res_short + 0.10,
            "program-dependent tail not slower after long burst: res_long={res_long:.3}, res_short={res_short:.3}"
        );

        // THD en cuerda Mi grave (82.41 Hz): sin modulación de onda (< 1.5%).
        let mut comp_thd = OptoCompressor::new();
        comp_thd.prepare(sr, 256);
        comp_thd.set_peak_reduction(60.0);
        comp_thd.set_mode(0.0);
        comp_thd.set_mix(100.0);
        comp_thd.set_gain_db(0.0);

        let warm = 48_000usize;
        let win = 8_192usize;
        let (mut sig_low, f_low) = generate_coherent_sine(82.41, sr, warm + win, 0.5);
        for ch in sig_low.chunks_mut(256) {
            comp_thd.process(ch);
        }
        let gr_low = comp_thd.gain_reduction_db();
        assert!(gr_low > 3.0, "expected GR > 3 dB at 82.41 Hz, got {gr_low}");
        let hm = compute_harmonic_metrics(
            &sig_low[warm..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_low),
        );
        assert!(
            hm.thd_percent < 1.5,
            "OptoComp low-frequency THD too high: {}%",
            hm.thd_percent
        );

        // Mix = 0% igual a señal seca * makeup; Limit produce más GR que Compress.
        let mut comp_mix = OptoCompressor::new();
        comp_mix.prepare(sr, 128);
        comp_mix.set_peak_reduction(90.0);
        comp_mix.set_gain_db(6.0);
        comp_mix.set_mix(0.0);
        comp_mix.reset();
        let dry: Vec<f32> = (0..1024).map(|i| 0.4 * (i as f32 * 0.1).sin()).collect();
        let mut out_mix = dry.clone();
        comp_mix.process(&mut out_mix);
        let g_mk = db_to_lin(6.0);
        let max_diff = dry
            .iter()
            .zip(out_mix.iter())
            .fold(0.0_f32, |m, (&x, &y)| m.max((y - x * g_mk).abs()));
        assert!(
            max_diff < 1e-5,
            "Mix=0 leaked wet signal: max_diff={max_diff}"
        );

        let measure_mode_gr = |mode_val: f32| -> f32 {
            let mut c = OptoCompressor::new();
            c.prepare(sr, 256);
            c.set_peak_reduction(70.0);
            c.set_mode(mode_val);
            c.set_gain_db(0.0);
            let mut b: Vec<f32> = (0..24_000)
                .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr).sin())
                .collect();
            for ch in b.chunks_mut(256) {
                c.process(ch);
            }
            c.gain_reduction_db()
        };
        assert!(measure_mode_gr(1.0) > measure_mode_gr(0.0) + 1.0);
    }

    #[test]
    fn brickwall_limiter_true_peak_zero_overshoot_and_fs_invariance() {
        let ceiling_db = -1.0_f32;
        let c_lin = db_to_lin(ceiling_db);

        // 1) ISP: seno a fs/4 (12 kHz @ 48 kHz) con fase pi/4 y amplitud 1.4 (+2.9 dBTP).
        let sr48 = 48_000.0_f32;
        let mut lim = BrickwallLimiter::new();
        lim.prepare(sr48, 256);
        lim.set_drive_db(0.0);
        lim.set_ceiling_db(ceiling_db);
        lim.set_release_ms(120.0);

        let n_isp = 24_000usize;
        let mut isp_buf: Vec<f32> = (0..n_isp)
            .map(|i| {
                let phase = std::f32::consts::FRAC_PI_4 + std::f32::consts::FRAC_PI_2 * (i as f32);
                1.4 * phase.sin()
            })
            .collect();
        for ch in isp_buf.chunks_mut(256) {
            lim.process(ch);
        }
        let steady = &isp_buf[12_000..];
        let max_steady = steady.iter().fold(0.0_f32, |m, &x| m.max(x.abs()));
        assert!(
            max_steady <= c_lin + 1e-6,
            "ISP steady peak exceeded ceiling: {max_steady} > {c_lin}"
        );
        assert!(
            max_steady <= 0.75 * c_lin,
            "sample peak did not reflect true-peak reduction (expected <= {:.4}, got {max_steady})",
            0.75 * c_lin
        );

        // Estima True-Peak de salida mediante interpolación sinc Hann 64 taps a 8x.
        let mut max_tp_out = 0.0_f64;
        for i in 40..(steady.len() - 40) {
            max_tp_out = max_tp_out.max((steady[i] as f64).abs());
            for p in 1..8 {
                let frac = p as f64 / 8.0;
                let mut acc = 0.0_f64;
                let mut wsum = 0.0_f64;
                for k in -31_isize..=32 {
                    let t = frac - k as f64;
                    let sinc = if t.abs() < 1e-12 {
                        1.0
                    } else {
                        (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
                    };
                    let win = 0.5 * (1.0 + (std::f64::consts::PI * t / 32.5).cos());
                    let h = sinc * win;
                    wsum += h;
                    acc += h * steady[(i as isize + k) as usize] as f64;
                }
                max_tp_out = max_tp_out.max((acc / wsum).abs());
            }
        }
        assert!(
            max_tp_out <= 1.02 * c_lin as f64,
            "output true-peak exceeded 1.02*C: {max_tp_out} > {}",
            1.02 * c_lin as f64
        );

        // 2) 0.00 dB overshoot + latencia exacta e invarianza en {44.1, 48, 96, 192 kHz} + release.
        for &(sr, expected_lat) in &[
            (44_100.0_f32, 74usize),
            (48_000.0, 80),
            (96_000.0, 152),
            (192_000.0, 288),
        ] {
            let mut l = BrickwallLimiter::new();
            l.prepare(sr, 256);
            assert_eq!(
                l.latency_samples(),
                expected_lat,
                "unexpected limiter latency at {sr} Hz"
            );
            let rel_ms = 80.0_f32;
            l.set_drive_db(6.0);
            l.set_ceiling_db(ceiling_db);
            l.set_release_ms(rel_ms);

            let seg = (0.15 * sr) as usize;
            let rel_tail = (0.40 * sr) as usize;
            let total = seg * 2 + rel_tail;
            let mut sig = Vec::with_capacity(total);
            for i in 0..total {
                let amp_db = if i < seg {
                    -20.0
                } else if i < 2 * seg {
                    6.0
                } else {
                    -30.0
                };
                let t = i as f32 / sr;
                let w = 0.6 * (2.0 * std::f32::consts::PI * 1_000.0 * t).sin()
                    + 0.4 * (2.0 * std::f32::consts::PI * 3_300.0 * t).sin();
                sig.push(db_to_lin(amp_db) * w);
            }
            let drive_lin = db_to_lin(6.0);
            let mut out = sig.clone();
            for ch in out.chunks_mut(256) {
                l.process(ch);
            }

            let max_all = out.iter().fold(0.0_f32, |m, &x| m.max(x.abs()));
            assert!(
                max_all <= c_lin + 1e-6,
                "overshoot at {sr} Hz: {max_all} > {c_lin}"
            );

            let check_start =
                2 * seg + ((3.0 * rel_ms * 1e-3) * sr).round() as usize + expected_lat;
            let check_end = (check_start + (0.04 * sr) as usize).min(total);
            let rms_out = compute_waveform_metrics(&out[check_start..check_end]).rms_db;
            let delayed_ref: Vec<f32> = sig
                [(check_start - expected_lat)..(check_end - expected_lat)]
                .iter()
                .map(|&x| x * drive_lin)
                .collect();
            let rms_ref = compute_waveform_metrics(&delayed_ref).rms_db;
            assert!(
                (rms_out - rms_ref).abs() < 0.5,
                "limiter did not recover within 0.5 dB at {sr} Hz: out={rms_out}, ref={rms_ref}"
            );
        }
    }

    #[test]
    fn soft_clip_tape_transfer_adaa_and_tape_hf_compression() {
        const WARMUP: usize = 2048;
        const N: usize = 4096;
        let sr = 48_000.0_f32;
        let ceiling_db = -1.0_f32;
        let c_lin = db_to_lin(ceiling_db);

        // 1) Soft Clip a bajo nivel (-30 dBFS, Drive 0 dB): lineal y THD < -80 dB.
        let mut sc = SoftClipTape::new();
        sc.prepare(sr, 256);
        sc.set_mode(0.0);
        sc.set_drive_db(0.0);
        sc.set_ceiling_db(ceiling_db);
        sc.set_knee_pct(50.0);

        let (low_s, f_low) = generate_coherent_sine(1_000.0, sr, WARMUP + N, db_to_lin(-30.0));
        let mut low_out = low_s.clone();
        for ch in low_out.chunks_mut(256) {
            sc.process(ch);
        }
        let rms_err = (compute_waveform_metrics(&low_out[WARMUP..]).rms_db
            - compute_waveform_metrics(&low_s[WARMUP..]).rms_db)
            .abs();
        assert!(
            rms_err < 0.05,
            "Soft Clip low-level gain error: {rms_err} dB"
        );
        let hm_low = compute_harmonic_metrics(
            &low_out[WARMUP..],
            sr,
            WindowKind::BlackmanHarris4,
            Some(f_low),
        );
        assert!(
            hm_low.thd_db < -80.0,
            "Soft Clip low-level THD too high: {} dB",
            hm_low.thd_db
        );

        // 2) Compresión hacia el techo: Drive 6 <= 1.08*C, Drive 24 <= 1.35*C, ΔRMS < 1.5 dB.
        let mut run_drive = |drv: f32| -> (f32, f32) {
            sc.reset();
            sc.set_drive_db(drv);
            let (s_in, _) = generate_coherent_sine(1_000.0, sr, WARMUP + N, 0.5);
            let mut s_out = s_in;
            for ch in s_out.chunks_mut(256) {
                sc.process(ch);
            }
            let max_v = s_out.iter().fold(0.0_f32, |m, &x| m.max(x.abs()));
            let rms = compute_waveform_metrics(&s_out[WARMUP..]).rms_db;
            (max_v, rms)
        };
        let (max6, _) = run_drive(6.0);
        let (_, rms12) = run_drive(12.0);
        let (max24, rms24) = run_drive(24.0);
        assert!(
            max6 <= 1.08 * c_lin,
            "Drive 6 peak {max6} > {}",
            1.08 * c_lin
        );
        assert!(
            max24 <= 1.35 * c_lin,
            "Drive 24 peak {max24} > {}",
            1.35 * c_lin
        );
        assert!(
            rms24 - rms12 < 1.5,
            "RMS did not saturate cleanly: rms24={rms24}, rms12={rms12}"
        );

        // 3) Rechazo de aliasing (> 20 dB vs hard clip 1x en 4370 Hz, Drive 18 dB) en 44.1 y 48 kHz.
        for &test_sr in &[44_100.0_f32, 48_000.0] {
            let (base_hi, f_hi) = generate_coherent_sine(4_370.0, test_sr, N, 0.5);
            let hi: Vec<f32> = base_hi.iter().copied().cycle().take(WARMUP + N).collect();
            let g18 = db_to_lin(18.0);
            let hard_1x: Vec<f32> = hi.iter().map(|&x| (x * g18).clamp(-c_lin, c_lin)).collect();
            let hm_hard = compute_harmonic_metrics(
                &hard_1x[WARMUP..],
                test_sr,
                WindowKind::BlackmanHarris4,
                Some(f_hi),
            );

            let mut sc_test = SoftClipTape::new();
            sc_test.prepare(test_sr, 256);
            sc_test.set_mode(0.0);
            sc_test.set_ceiling_db(ceiling_db);
            sc_test.set_knee_pct(50.0);
            sc_test.set_drive_db(18.0);
            let mut sc_hi = hi;
            for ch in sc_hi.chunks_mut(256) {
                sc_test.process(ch);
            }
            let hm_sc = compute_harmonic_metrics(
                &sc_hi[WARMUP..],
                test_sr,
                WindowKind::BlackmanHarris4,
                Some(f_hi),
            );
            let asnr_gain = hm_sc.aliasing_snr_db - hm_hard.aliasing_snr_db;
            assert!(
                asnr_gain > 20.0,
                "SoftClip aliasing improvement < 20 dB at {test_sr} Hz: +{asnr_gain:.2} dB (hard={:.2}, sc={:.2})",
                hm_hard.aliasing_snr_db,
                hm_sc.aliasing_snr_db
            );
        }

        // 4) Tape: lineal/plano a -40 dBFS; compresión en agudos a +12 dB drive; H2 > -50 dB.
        let mut tape = SoftClipTape::new();
        tape.prepare(sr, 256);
        tape.set_mode(1.0);
        tape.set_ceiling_db(ceiling_db);
        tape.set_knee_pct(75.0);

        let gain_at =
            |tp: &mut SoftClipTape, freq: f32, amp: f32, drv: f32| -> (f32, HarmonicMetrics) {
                tp.reset();
                tp.set_drive_db(drv);
                let (s_in, f_ex) = generate_coherent_sine(freq, sr, WARMUP + N, amp);
                let in_db = compute_waveform_metrics(&s_in[WARMUP..]).rms_db;
                let mut s_out = s_in;
                for ch in s_out.chunks_mut(256) {
                    tp.process(ch);
                }
                let out_db = compute_waveform_metrics(&s_out[WARMUP..]).rms_db;
                let hm = compute_harmonic_metrics(
                    &s_out[WARMUP..],
                    sr,
                    WindowKind::BlackmanHarris4,
                    Some(f_ex),
                );
                (out_db - in_db, hm)
            };

        let (g_200_low, _) = gain_at(&mut tape, 200.0, db_to_lin(-40.0), 0.0);
        let (g_6k_low, _) = gain_at(&mut tape, 6_000.0, db_to_lin(-40.0), 0.0);
        assert!(
            (g_6k_low - g_200_low).abs() < 0.5,
            "Tape not flat at -40 dBFS: 200Hz={g_200_low}, 6kHz={g_6k_low}"
        );

        let (g_200_hot, _) = gain_at(&mut tape, 200.0, db_to_lin(-12.0), 12.0);
        let (g_6k_hot, _) = gain_at(&mut tape, 6_000.0, db_to_lin(-12.0), 12.0);
        assert!(
            g_200_hot - g_6k_hot > 1.5,
            "Tape HF compression too weak: 200Hz={g_200_hot}, 6kHz={g_6k_hot}"
        );

        let (_, hm_tape) = gain_at(&mut tape, 1_000.0, db_to_lin(-12.0), 12.0);
        assert!(
            hm_tape.h2_db > -50.0,
            "Tape should generate even harmonic H2 > -50 dB: {}",
            hm_tape.h2_db
        );
    }

    #[test]
    fn parametric_eq_matched_response_no_nyquist_cramping() {
        let mut eq = ParametricEq::new();
        assert_eq!(eq.latency_samples(), 0);

        // Verifica precisión de ganancia, simetría boost/cut y ausencia de aplastamiento cerca de Nyquist.
        let measure_sine_gain_db = |fx: &mut ParametricEq, sr: f32, freq_hz: f32| -> (f32, f32) {
            fx.reset();
            let warmup = (0.08 * sr) as usize;
            let n = if sr >= 96_000.0 { 8192usize } else { 4096usize };
            let (base, f_exact) = generate_coherent_sine(freq_hz, sr, n, 0.2);
            let s_in: Vec<f32> = base.iter().copied().cycle().take(warmup + n).collect();
            let mut s_out = s_in.clone();
            for ch in s_out.chunks_mut(256) {
                fx.process(ch);
            }
            let in_db = compute_waveform_metrics(&s_in[warmup..]).rms_db;
            let out_db = compute_waveform_metrics(&s_out[warmup..]).rms_db;
            (out_db - in_db, f_exact)
        };

        for &sr in &[44_100.0_f32, 48_000.0, 96_000.0, 192_000.0] {
            // 1) Low Shelf 100 Hz +/- 12 dB -> en esquina f0 = +6 dB; en grave profundo ~ +12 dB.
            eq.set_low_band(100.0, 12.0);
            eq.set_low_mid_band(650.0, 0.0, 1.5);
            eq.set_high_mid_band(8_000.0, 0.0, 1.5);
            eq.set_high_band(10_000.0, 0.0);
            eq.set_level_db(0.0);
            eq.prepare(sr, 256);

            let (ls_corner, f_ls) = measure_sine_gain_db(&mut eq, sr, 100.0);
            let ls_expected = eq.magnitude_db_at(f_ls);
            let ls_nominal = eq.magnitude_db_at(100.0);
            assert!(
                (ls_nominal - 6.0).abs() < 0.15 && (ls_corner - ls_expected).abs() < 0.25,
                "LowShelf corner gain at {sr} Hz (f={f_ls:.1}): nom100={ls_nominal:.2}, meas={ls_corner:.2}, exp={ls_expected:.2} dB"
            );

            // 2) Bandas Bell 650 Hz (+12 / -12 dB) y 8 kHz (+12 dB) + simetría boost/cut.
            eq.set_low_band(100.0, 0.0);
            eq.set_low_mid_band(650.0, 12.0, 2.0);
            eq.prepare(sr, 256);
            let (bell_650_boost, f_650) = measure_sine_gain_db(&mut eq, sr, 650.0);
            let exp_650 = eq.magnitude_db_at(f_650);
            let nom_650 = eq.magnitude_db_at(650.0);
            assert!(
                (nom_650 - 12.0).abs() < 0.15 && (bell_650_boost - exp_650).abs() < 0.25,
                "LM Bell 650 Hz boost at {sr} Hz: nom={nom_650:.2}, meas={bell_650_boost:.2}, exp={exp_650:.2} dB"
            );

            eq.set_low_mid_band(650.0, -12.0, 2.0);
            eq.prepare(sr, 256);
            let (bell_650_cut, _) = measure_sine_gain_db(&mut eq, sr, 650.0);
            assert!(
                (bell_650_boost + bell_650_cut).abs() < 0.1,
                "Bell boost/cut asymmetry at {sr} Hz: +{bell_650_boost:.2} vs {bell_650_cut:.2}"
            );

            // 3) HM Bell a 8 kHz (cerca de Nyquist en 44.1k): ganancia exacta y forma preservada.
            eq.set_low_mid_band(650.0, 0.0, 1.5);
            eq.set_high_mid_band(8_000.0, 12.0, 1.5);
            eq.prepare(sr, 256);
            let (bell_8k, f_8k) = measure_sine_gain_db(&mut eq, sr, 8_000.0);
            let exp_8k = eq.magnitude_db_at(f_8k);
            let nom_8k = eq.magnitude_db_at(8_000.0);
            assert!(
                (nom_8k - 12.0).abs() < 0.15 && (bell_8k - exp_8k).abs() < 0.25,
                "HM Bell 8 kHz gain at {sr} Hz: nom={nom_8k:.2}, meas={bell_8k:.2}, exp={exp_8k:.2} dB"
            );

            // 4) High Shelf 10 kHz +12 dB: mitad en 10 kHz (~+6 dB) incluso a 44.1 kHz.
            eq.set_high_mid_band(2_200.0, 0.0, 1.2);
            eq.set_high_band(10_000.0, 12.0);
            eq.prepare(sr, 256);
            let (hs_10k, f_hs) = measure_sine_gain_db(&mut eq, sr, 10_000.0);
            let exp_hs = eq.magnitude_db_at(f_hs);
            let nom_hs = eq.magnitude_db_at(10_000.0);
            assert!(
                (nom_hs - 6.0).abs() < 0.15 && (hs_10k - exp_hs).abs() < 0.25,
                "HighShelf 10 kHz corner at {sr} Hz: nom={nom_hs:.2}, meas={hs_10k:.2}, exp={exp_hs:.2} dB"
            );
        }

        // Compara ancho de campana a 8 kHz (en 11.5 kHz) entre 44.1 kHz y 192 kHz -> sin cramping.
        let mut eq_44 = ParametricEq::new();
        eq_44.set_high_mid_band(8_000.0, 12.0, 1.5);
        eq_44.prepare(44_100.0, 256);
        let mut eq_192 = ParametricEq::new();
        eq_192.set_high_mid_band(8_000.0, 12.0, 1.5);
        eq_192.prepare(192_000.0, 256);
        let skirt_diff = (eq_44.magnitude_db_at(11_500.0) - eq_192.magnitude_db_at(11_500.0)).abs();
        assert!(
            skirt_diff < 0.6,
            "Nyquist cramping detected between 44.1k and 192k at 11.5 kHz: diff={skirt_diff:.2} dB"
        );
    }

    #[test]
    fn wah_crybaby_coupled_q_and_auto_envelope_modes() {
        let sr = 48_000.0_f32;
        let mut wah = Wah::new();
        assert_eq!(wah.latency_samples(), 0);
        wah.set_mode(0.0); // Manual
        wah.set_resonance(70.0);
        wah.set_mix(100.0);

        let find_peak_hz = |w: &mut Wah| -> (f32, f32) {
            w.reset();
            let n = 4096usize;
            let mut imp = vec![0.0_f32; n];
            imp[0] = 1.0;
            for ch in imp.chunks_mut(128) {
                w.process(ch);
            }
            let mut spec = vec![-120.0_f32; n / 2];
            compute_spectrum_db_with_window(&imp, &mut spec, WindowKind::Hann);
            // FFT directa sobre impulso sin enventanado alteraría inicio; usamos ruido blanco/multitono si no,
            // aquí usamos compute_spectrum_db sobre una respuesta desplazada en n=512!
            w.reset();
            let mut imp_mid = vec![0.0_f32; n];
            imp_mid[512] = 0.5;
            for ch in imp_mid.chunks_mut(128) {
                w.process(ch);
            }
            compute_spectrum_db(&imp_mid, &mut spec);
            let bin_hz = sr / n as f32;
            let k_min = (250.0 / bin_hz).ceil() as usize;
            let k_max = (4_000.0 / bin_hz).floor() as usize;
            let (peak_k, &peak_db) = spec[k_min..=k_max]
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
                .unwrap();
            let abs_k = k_min + peak_k;
            // Mide ancho de banda relativo -6 dB alrededor del pico para verificar aumento de Q.
            let thresh = peak_db - 6.0;
            let mut lo_k = abs_k;
            while lo_k > k_min && spec[lo_k] > thresh {
                lo_k -= 1;
            }
            let mut hi_k = abs_k;
            while hi_k < k_max && spec[hi_k] > thresh {
                hi_k += 1;
            }
            let f_peak = abs_k as f32 * bin_hz;
            let bw_rel = ((hi_k - lo_k).max(1) as f32 * bin_hz) / f_peak.max(1.0);
            (f_peak, bw_rel)
        };

        wah.set_position(0.0);
        wah.prepare(sr, 128);
        let (f_heel, bw_heel) = find_peak_hz(&mut wah);

        wah.set_position(100.0);
        wah.prepare(sr, 128);
        let (f_toe, bw_toe) = find_peak_hz(&mut wah);

        assert!(
            (300.0..=440.0).contains(&f_heel),
            "Heel peak frequency out of CryBaby range: {f_heel} Hz"
        );
        assert!(
            (1_900.0..=2_450.0).contains(&f_toe),
            "Toe peak frequency out of CryBaby range: {f_toe} Hz"
        );
        assert!(
            bw_toe < bw_heel,
            "Coupled Q should narrow relative bandwidth toward toe: bw_heel={bw_heel:.3}, bw_toe={bw_toe:.3}"
        );

        // Auto Up vs Auto Down con señal armónica rica (220 Hz + 880 Hz + 1900 Hz).
        let make_chord = |amp: f32, len: usize| -> Vec<f32> {
            (0..len)
                .map(|i| {
                    let t = i as f32 / sr;
                    amp * (0.4 * (std::f32::consts::TAU * 350.0 * t).sin()
                        + 0.35 * (std::f32::consts::TAU * 900.0 * t).sin()
                        + 0.35 * (std::f32::consts::TAU * 1_900.0 * t).sin())
                })
                .collect()
        };
        let spectral_tilt_ratio = |buf: &[f32]| -> f32 {
            let n = 2048usize;
            let mut spec = vec![-120.0_f32; n / 2];
            compute_spectrum_db(&buf[buf.len() - n..], &mut spec);
            let bin_hz = sr / n as f32;
            let k_lo = (350.0 / bin_hz).round() as usize;
            let k_hi = (1_900.0 / bin_hz).round() as usize;
            spec[k_hi] - spec[k_lo]
        };

        // Auto Up: señal fuerte debe favorecer agudos (1900 Hz) frente a señal débil.
        wah.set_mode(1.0);
        wah.set_position(10.0);
        wah.set_sensitivity(80.0);
        wah.prepare(sr, 128);
        let mut quiet = make_chord(0.01, 4096);
        wah.process(&mut quiet);
        let ratio_quiet_up = spectral_tilt_ratio(&quiet);

        wah.reset();
        let mut hot = make_chord(0.7, 4096);
        wah.process(&mut hot);
        let ratio_hot_up = spectral_tilt_ratio(&hot);
        assert!(
            ratio_hot_up > ratio_quiet_up + 4.0,
            "Auto Up should sweep upward with hot signal: quiet={ratio_quiet_up:.2}, hot={ratio_hot_up:.2}"
        );

        // Auto Down: señal fuerte barre hacia abajo respecto a señal débil.
        wah.set_mode(2.0);
        wah.set_position(95.0);
        wah.prepare(sr, 128);
        let mut quiet_d = make_chord(0.01, 4096);
        wah.process(&mut quiet_d);
        let ratio_quiet_down = spectral_tilt_ratio(&quiet_d);

        wah.reset();
        let mut hot_d = make_chord(0.7, 4096);
        wah.process(&mut hot_d);
        let ratio_hot_down = spectral_tilt_ratio(&hot_d);
        assert!(
            ratio_hot_down < ratio_quiet_down - 4.0,
            "Auto Down should sweep downward with hot signal: quiet={ratio_quiet_down:.2}, hot={ratio_hot_down:.2}"
        );

        // Conmutación en vivo Manual -> Auto Up -> Auto Down sin NaN ni saltos bruscos.
        wah.prepare(sr, 64);
        let mut sweep_sig = make_chord(0.35, 2048);
        for (blk_idx, ch) in sweep_sig.chunks_mut(64).enumerate() {
            if blk_idx == 8 {
                wah.set_mode(0.0);
            } else if blk_idx == 16 {
                wah.set_mode(2.0);
            }
            wah.process(ch);
            assert!(ch.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn cabinet_ir_upols_zero_latency_and_exact_vs_direct_convolution() {
        let sr = 48_000.0_f32;
        let ir_len = 2_500usize;
        let ir: Vec<f32> = (0..ir_len)
            .map(|k| {
                let env = (-(k as f32) / 380.0).exp();
                let osc = (k as f32 * 0.27).cos() * 0.6 + (k as f32 * 0.81).sin() * 0.4;
                let impulse = if k == 0 { 0.45 } else { 0.0 };
                impulse + env * osc * 0.15
            })
            .collect();

        // 1) Verifica latencia exacta = 0 muestras en CabinetIr + UpolsConvolver.
        let mut cab = CabinetIr::new();
        assert_eq!(cab.latency_samples(), 0);
        cab.prepare(sr, 64);
        cab.set_low_cut_hz(20.0);
        cab.set_high_cut_hz(18_000.0);
        cab.set_mix(100.0);
        cab.set_level_db(0.0);
        cab.load_test_ir_unscaled(&ir);
        let mut imp = vec![0.0_f32; 128];
        imp[0] = 1.0;
        cab.process(&mut imp);
        assert!(
            imp[0].abs() > 0.05,
            "CabinetIr must output immediately at sample 0 (0-latency): y[0]={}",
            imp[0]
        );

        // 2) Identidad matemática exacta de UpolsConvolver vs convolución directa en tiempo (ESR < -115 dB).
        let mut conv = UpolsConvolver::new(sr);
        let parts = PreparedIrPartitions::from_ir(&ir, sr);
        conv.load_partitions(&parts);

        let n = 8_192usize;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                0.4 * (std::f32::consts::TAU * 190.0 * t).sin()
                    + 0.35 * (std::f32::consts::TAU * 830.0 * t).cos()
                    + 0.25 * (std::f32::consts::TAU * 2_910.0 * t).sin()
            })
            .collect();

        // Convolución directa de referencia en f64.
        let mut y_ref = vec![0.0_f32; n];
        for (idx, dst) in y_ref.iter_mut().enumerate() {
            let mut acc = 0.0_f64;
            let k_max = ir.len().min(idx + 1);
            for k in 0..k_max {
                acc += (ir[k] as f64) * (input[idx - k] as f64);
            }
            *dst = acc as f32;
        }

        // Procesa en bloques irregulares [1, 17, 64, 128, 511] para validar invarianza al tamaño de buffer.
        let mut y_upols = input.clone();
        let sizes = [1usize, 17, 64, 128, 511];
        let mut offset = 0usize;
        let mut s_idx = 0usize;
        while offset < n {
            let chunk_len = sizes[s_idx % sizes.len()].min(n - offset);
            for s in &mut y_upols[offset..offset + chunk_len] {
                *s = conv.process_sample(*s);
            }
            offset += chunk_len;
            s_idx += 1;
        }

        assert!(
            (y_upols[0] - y_ref[0]).abs() < 1e-6,
            "Sample 0 mismatch: upols={} ref={}",
            y_upols[0],
            y_ref[0]
        );
        let esr_db = compute_esr_db(&y_ref, &y_upols);
        println!(
            "UPOLS_BENCH: ir_len={ir_len} block_b={} esr_db={esr_db:.2}",
            conv.block_size()
        );
        assert!(
            esr_db < -115.0,
            "UPOLS vs direct convolution ESR too high: {esr_db:.2} dB"
        );
    }

    #[test]
    fn cabinet_ir_wav_roundtrip_resample_and_builtin_rolloff() {
        // 1) Roundtrip WAV 96 kHz -> parser -> resample sinc 48 kHz -> normalización en banda.
        let sr_in = 96_000u32;
        let sr_out = 48_000u32;
        let mut raw_ir = vec![0.0_f32; 2400];
        raw_ir[0] = 0.2;
        for (i, s) in raw_ir.iter_mut().enumerate().skip(1) {
            let t = i as f32 / sr_in as f32;
            *s = 0.08 * (-t * 95.0).exp() * (std::f32::consts::TAU * 650.0 * t).cos();
        }
        let wav_bytes = wav::write_wav_f32_mono(sr_in, &raw_ir);
        let parsed = wav::parse_wav_bytes(&wav_bytes).expect("parse generated wav");
        assert_eq!(parsed.sample_rate, sr_in);
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.samples.len(), raw_ir.len());

        let mut resampled = wav::resample_ir_sinc(&parsed.samples, sr_in, sr_out, 4800);
        assert_eq!(resampled.len(), 1200);
        wav::normalize_ir_guitar_band(&mut resampled, sr_out as f32);
        let parts = PreparedIrPartitions::from_ir(&resampled, sr_out as f32);
        assert_eq!(parts.block_size, 64);

        // 2) Los 3 modelos integrados deben atenuar severamente >= 18 dB a 10 kHz vs 1 kHz en 44.1-192 kHz.
        let mut cab = CabinetIr::new();
        for &sr in &[44_100.0_f32, 48_000.0, 96_000.0, 192_000.0] {
            for model in 0..3 {
                cab.set_model(model as f32);
                cab.set_low_cut_hz(20.0);
                cab.set_high_cut_hz(18_000.0);
                cab.set_mix(100.0);
                cab.set_level_db(0.0);
                cab.prepare(sr, 256);

                let measure_rms = |c: &mut CabinetIr, freq: f32| -> f32 {
                    c.reset();
                    let warmup = (0.06 * sr) as usize;
                    let n = 4096usize;
                    let (mut sig, _) = generate_coherent_sine(freq, sr, warmup + n, 0.25);
                    for ch in sig.chunks_mut(256) {
                        c.process(ch);
                    }
                    compute_waveform_metrics(&sig[warmup..]).rms_db
                };

                let rms_1k = measure_rms(&mut cab, 1_000.0);
                let rms_10k = measure_rms(&mut cab, 10_000.0);
                let atten_db = rms_1k - rms_10k;
                assert!(
                    atten_db >= 18.0,
                    "Cabinet model {model} @ {sr} Hz insufficient HF rolloff: {atten_db:.2} dB (1k={rms_1k:.2}, 10k={rms_10k:.2})"
                );
            }
        }
    }

    #[test]
    fn chorus_hermite_modulation_smooth_and_bbd_warmth() {
        let sr = 48_000.0_f32;
        let mut ch = Chorus::new();
        assert_eq!(ch.latency_samples(), 0);
        ch.set_rate_hz(1.5);
        ch.set_depth(80.0);
        ch.set_center_ms(8.0);
        ch.set_warmth_hz(5_000.0);
        ch.set_mix(100.0);
        ch.prepare(sr, 256);

        // 1) Suavidad temporal sin saltos discontinuos bajo modulación Hermite en seno puro.
        let (mut sig, _) = generate_coherent_sine(440.0, sr, 24_000, 0.35);
        for blk in sig.chunks_mut(128) {
            ch.process(blk);
        }
        let max_step = sig[4096..]
            .windows(2)
            .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(
            max_step < 0.08,
            "Chorus step discontinuity too high: max_step={max_step}"
        );

        // 2) Calidez BBD: en 14 kHz debe atenuar claramente respecto a 1 kHz.
        let measure_rms = |c: &mut Chorus, freq: f32| -> f32 {
            c.reset();
            let warmup = 4096usize;
            let n = 4096usize;
            let (mut s, _) = generate_coherent_sine(freq, sr, warmup + n, 0.25);
            for blk in s.chunks_mut(256) {
                c.process(blk);
            }
            compute_waveform_metrics(&s[warmup..]).rms_db
        };
        let rms_1k = measure_rms(&mut ch, 1_000.0);
        let rms_14k = measure_rms(&mut ch, 14_000.0);
        assert!(
            rms_1k - rms_14k > 10.0,
            "Chorus BBD warmth filter too weak: 1k={rms_1k:.2}, 14k={rms_14k:.2}"
        );
    }

    #[test]
    fn phaser_tpt_notches_move_and_stages_crossfade() {
        let sr = 48_000.0_f32;
        let mut ph = Phaser::new();
        assert_eq!(ph.latency_samples(), 0);
        ph.set_depth(95.0);
        ph.set_feedback(85.0);
        ph.set_mix(50.0);

        for &stages in &[0.0_f32, 1.0] {
            ph.set_stages(stages);
            ph.set_rate_hz(1.2);
            ph.prepare(sr, 256);

            // Estabilidad acotada incluso con feedback máximo 85% y ruido/acorde.
            let mut probe: Vec<f32> = (0..48_000)
                .map(|i| {
                    let t = i as f32 / sr;
                    0.3 * (std::f32::consts::TAU * 220.0 * t).sin()
                        + 0.25 * (std::f32::consts::TAU * 880.0 * t).sin()
                        + 0.2 * (std::f32::consts::TAU * 2_200.0 * t).sin()
                })
                .collect();
            for blk in probe.chunks_mut(128) {
                ph.process(blk);
                assert!(blk.iter().all(|v| v.is_finite() && v.abs() < 2.5));
            }
        }

        // Verifica muesca móvil profunda (> 10 dB) deteniendo LFO y barriendo un impulso desplazado.
        ph.set_stages(0.0);
        ph.set_rate_hz(0.08);
        ph.set_feedback(0.0);
        ph.set_mix(50.0);
        ph.prepare(sr, 256);
        let mut imp = vec![0.0_f32; 4096];
        imp[256] = 0.8;
        ph.process(&mut imp);
        let mut spec = vec![-120.0_f32; 2048];
        compute_spectrum_db(&imp, &mut spec);
        let bin_hz = sr / 4096.0;
        let k_lo = (150.0 / bin_hz).ceil() as usize;
        let k_hi = (4_500.0 / bin_hz).floor() as usize;
        let max_db = spec[k_lo..=k_hi].iter().copied().fold(-120.0_f32, f32::max);
        let min_db = spec[k_lo..=k_hi].iter().copied().fold(0.0_f32, f32::min);
        assert!(
            max_db - min_db > 10.0,
            "Phaser should create deep spectrum notches (> 10 dB): max={max_db:.2}, min={min_db:.2}"
        );

        // Conmutación en vivo 4-Stage <-> 6-Stage sin clics.
        ph.set_rate_hz(0.8);
        ph.prepare(sr, 64);
        let mut sig: Vec<f32> = (0..2048)
            .map(|i| 0.35 * (std::f32::consts::TAU * 330.0 * i as f32 / sr).sin())
            .collect();
        for (idx, ch) in sig.chunks_mut(64).enumerate() {
            if idx == 8 {
                ph.set_stages(1.0);
            } else if idx == 20 {
                ph.set_stages(0.0);
            }
            ph.process(ch);
            assert!(ch.iter().all(|v| v.is_finite()));
        }
        let max_jump = sig
            .windows(2)
            .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(max_jump < 0.08, "Phaser stage switch jump: {max_jump}");
    }

    #[test]
    fn tremolo_optical_depth_and_harmonic_allpass_crossover() {
        let sr = 48_000.0_f32;
        let mut tr = Tremolo::new();
        assert_eq!(tr.latency_samples(), 0);

        // 1) Harmonic con Depth = 0% debe sumar a magnitud plana (±0.25 dB) gracias al crossover LR2.
        tr.set_mode(1.0);
        tr.set_depth(0.0);
        tr.set_level_db(0.0);
        tr.prepare(sr, 256);
        for &f in &[150.0_f32, 700.0, 2_800.0, 7_500.0] {
            tr.reset();
            let warmup = 4096usize;
            let n = 4096usize;
            let (s_in, _) = generate_coherent_sine(f, sr, warmup + n, 0.4);
            let mut s_out = s_in.clone();
            tr.process(&mut s_out);
            let diff = (compute_waveform_metrics(&s_out[warmup..]).rms_db
                - compute_waveform_metrics(&s_in[warmup..]).rms_db)
                .abs();
            assert!(
                diff < 0.25,
                "Harmonic crossover magnitude error at {f} Hz with 0% depth: {diff:.3} dB"
            );
        }

        // 2) Harmonic con Depth = 90%: envolvente de graves (200 Hz) y agudos (2.5 kHz) en contrafase.
        tr.set_rate_hz(4.0);
        tr.set_depth(90.0);
        tr.prepare(sr, 256);
        let half_period = (0.5 * sr / 4.0) as usize; // t0 vs t0 + T/2
        let (mut s_lo, _) = generate_coherent_sine(200.0, sr, 48_000, 0.5);
        tr.process(&mut s_lo);
        tr.reset();
        let (mut s_hi, _) = generate_coherent_sine(2_500.0, sr, 48_000, 0.5);
        tr.process(&mut s_hi);

        let w = 1024usize;
        let lo_q1 = compute_waveform_metrics(&s_lo[3000..3000 + w]).rms_db;
        let lo_q2 =
            compute_waveform_metrics(&s_lo[3000 + half_period..3000 + half_period + w]).rms_db;
        let hi_q1 = compute_waveform_metrics(&s_hi[3000..3000 + w]).rms_db;
        let hi_q2 =
            compute_waveform_metrics(&s_hi[3000 + half_period..3000 + half_period + w]).rms_db;
        assert!(
            (lo_q1 - lo_q2) * (hi_q1 - hi_q2) < -10.0,
            "Harmonic bands should modulate in opposite phase: lo=({lo_q1:.2}, {lo_q2:.2}), hi=({hi_q1:.2}, {hi_q2:.2})"
        );

        // 3) Modo Optical genera modulación de amplitud profunda (> 8 dB swing).
        tr.set_mode(0.0);
        tr.prepare(sr, 256);
        let (mut s_opt, _) = generate_coherent_sine(440.0, sr, 48_000, 0.5);
        tr.process(&mut s_opt);
        let opt_q1 = compute_waveform_metrics(&s_opt[3000..3000 + w]).rms_db;
        let opt_q2 =
            compute_waveform_metrics(&s_opt[3000 + half_period..3000 + half_period + w]).rms_db;
        assert!(
            (opt_q1 - opt_q2).abs() > 8.0,
            "Optical tremolo modulation too weak: {opt_q1:.2} vs {opt_q2:.2}"
        );
    }

    #[test]
    fn delay_digital_bbd_tape_progressive_darkening_and_glide() {
        let sr = 48_000.0_f32;
        let mut d = Delay::new();
        assert_eq!(d.latency_samples(), 0);

        // 1) Verifica eco discreto en Time=60 ms (2880 muestras) y oscurecimiento BBD/Tape > Digital.
        let measure_echo_hf_drop = |mode: f32| -> (f32, f32) {
            let mut del = Delay::new();
            del.set_mode(mode);
            del.set_time_ms(60.0);
            del.set_feedback(65.0);
            del.set_tone(45.0);
            del.set_mod_depth(0.0);
            del.set_mix(100.0);
            del.prepare(sr, 256);

            // Inyecta ráfaga breve de dos tonos (250 Hz + 3500 Hz) durante 20 ms y observa echo #1 y #3.
            let len = 14_400usize; // 300 ms
            let mut buf = vec![0.0_f32; len];
            for (i, dst) in buf[..960].iter_mut().enumerate() {
                let t = i as f32 / sr;
                let win = 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / 960.0).cos());
                *dst = win
                    * 0.35
                    * ((std::f32::consts::TAU * 250.0 * t).sin()
                        + (std::f32::consts::TAU * 3_500.0 * t).sin());
            }
            for blk in buf.chunks_mut(256) {
                del.process(blk);
            }
            let echo_rms = |echo_num: usize| -> (f32, f32) {
                let start = echo_num * 2880;
                let slice = &buf[start..start + 1024];
                let mut spec = vec![-120.0_f32; 512];
                compute_spectrum_db(slice, &mut spec);
                let bin_hz = sr / 1024.0;
                let k_lo = (250.0 / bin_hz).round() as usize;
                let k_hi = (3_500.0 / bin_hz).round() as usize;
                (spec[k_lo], spec[k_hi])
            };
            let (e1_lo, e1_hi) = echo_rms(1);
            let (e3_lo, e3_hi) = echo_rms(3);
            let drop_hi_vs_lo_echo3 = (e3_lo - e3_hi) - (e1_lo - e1_hi);
            (e1_lo, drop_hi_vs_lo_echo3)
        };

        let (dig_e1, dig_drop) = measure_echo_hf_drop(0.0);
        let (bbd_e1, bbd_drop) = measure_echo_hf_drop(1.0);
        let (tape_e1, tape_drop) = measure_echo_hf_drop(2.0);
        assert!(dig_e1 > -35.0 && bbd_e1 > -35.0 && tape_e1 > -35.0);
        assert!(
            bbd_drop > dig_drop + 4.0,
            "Analog BBD should darken repeats faster than Digital: bbd_drop={bbd_drop:.2}, dig_drop={dig_drop:.2}"
        );
        assert!(
            tape_drop > dig_drop + 3.0,
            "Tape Delay should darken repeats faster than Digital: tape_drop={tape_drop:.2}, dig_drop={dig_drop:.2}"
        );

        // 2) Deslizamiento continuo de Time sin clics + estabilidad acotada con Feedback 95%.
        d.set_mode(2.0);
        d.set_time_ms(120.0);
        d.set_feedback(95.0);
        d.set_mix(50.0);
        d.prepare(sr, 64);
        let mut glide_buf = vec![0.0_f32; 16_384];
        for (i, dst) in glide_buf.iter_mut().enumerate() {
            let t = i as f32 / sr;
            *dst = 0.25 * (std::f32::consts::TAU * 330.0 * t).sin();
        }
        for (blk_i, blk) in glide_buf.chunks_mut(64).enumerate() {
            if blk_i == 40 {
                d.set_time_ms(320.0);
            }
            d.process(blk);
            assert!(blk.iter().all(|v| v.is_finite() && v.abs() < 3.0));
        }
        let max_jump = glide_buf
            .windows(2)
            .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(max_jump < 0.25, "Delay time glide click: {max_jump}");
    }

    #[test]
    fn reverb_plate_hall_spring_energy_stability_denormals_and_dispersion() {
        // 1) Estabilidad energética con Decay = 100% y decaimiento sin NaN/denormals en 48k y 192k.
        for &sr in &[48_000.0_f32, 192_000.0] {
            for &mode in &[0.0_f32, 1.0, 2.0] {
                let mut rev = Reverb::new();
                assert_eq!(rev.latency_samples(), 0);
                rev.set_mode(mode);
                rev.set_decay(100.0);
                rev.set_predelay_ms(20.0);
                rev.set_tone(65.0);
                rev.set_mix(100.0);
                rev.prepare(sr, 256);

                let burst_len = (0.15 * sr) as usize;
                let tail_len = (1.5 * sr) as usize;
                let mut burst = vec![0.0_f32; burst_len];
                for (i, dst) in burst.iter_mut().enumerate() {
                    let t = i as f32 / sr;
                    *dst = 0.5
                        * ((std::f32::consts::TAU * 220.0 * t).sin()
                            + 0.6 * (std::f32::consts::TAU * 1_100.0 * t).sin());
                }
                for blk in burst.chunks_mut(256) {
                    rev.process(blk);
                }
                let early_tail_rms = compute_waveform_metrics(&burst[burst_len - 512..]).rms;

                let mut tail = vec![0.0_f32; tail_len];
                for blk in tail.chunks_mut(256) {
                    rev.process(blk);
                    assert!(blk.iter().all(|v| v.is_finite() && v.abs() < 4.0));
                }
                let end_tail_rms = compute_waveform_metrics(&tail[tail_len - 1024..]).rms;
                assert!(
                    end_tail_rms < early_tail_rms.max(0.05) * 1.05,
                    "Reverb mode {mode} @ {sr} Hz must not blow up with Decay=100%: early={early_tail_rms}, end={end_tail_rms}"
                );
            }
        }

        // 2) Dispersión de grupo característica en Spring: banda 3–4.2 kHz llega posterior a 250–600 Hz.
        let sr = 48_000.0_f32;
        let mut spring = Reverb::new();
        spring.set_mode(2.0);
        spring.set_decay(65.0);
        spring.set_predelay_ms(0.0);
        spring.set_tone(90.0);
        spring.set_mix(100.0);
        spring.prepare(sr, 256);

        let n_imp = 2048usize;
        let mut imp = vec![0.0_f32; n_imp];
        imp[0] = 0.8;
        spring.process(&mut imp);

        // Filtra respuesta al impulso de Spring en banda grave (250–600 Hz) y banda dispersiva (3–4.2 kHz)
        // y calcula centroide temporal de energía (donde solo cae la cascada dispersiva del primer viaje).
        let energy_centroid_samples = |lo_hz: f32, hi_hz: f32| -> f64 {
            let mut hp = SvfTpt::new(sr);
            let mut lp = SvfTpt::new(sr);
            hp.set_params(SvfMode::HighPass, lo_hz, 0.707, 0.0);
            lp.set_params(SvfMode::LowPass, hi_hz, 0.707, 0.0);
            let mut num = 0.0_f64;
            let mut den = 0.0_f64;
            // Primer rebote (primeros ~25 ms = 1200 muestras).
            for (i, &x) in imp[..1200].iter().enumerate() {
                let bp = lp.process_sample(hp.process_sample(x));
                let e = (bp as f64) * (bp as f64);
                num += (i as f64) * e;
                den += e;
            }
            num / den.max(1e-18)
        };
        let t_low = energy_centroid_samples(250.0, 600.0);
        let t_high = energy_centroid_samples(3_000.0, 4_200.0);
        assert!(
            t_high > t_low + 40.0,
            "Spring dispersive all-pass must delay 3-4.2 kHz vs 250-600 Hz: t_low={t_low:.1}, t_high={t_high:.1}"
        );

        // 3) Conmutación sin clics Plate -> Hall -> Spring.
        let mut rev = Reverb::new();
        rev.prepare(sr, 64);
        let mut sig: Vec<f32> = (0..4096)
            .map(|i| 0.3 * (std::f32::consts::TAU * 330.0 * i as f32 / sr).sin())
            .collect();
        for (idx, blk) in sig.chunks_mut(64).enumerate() {
            if idx == 10 {
                rev.set_mode(1.0);
            } else if idx == 30 {
                rev.set_mode(2.0);
            }
            rev.process(blk);
            assert!(blk.iter().all(|v| v.is_finite()));
        }
        let max_jump = sig
            .windows(2)
            .fold(0.0_f32, |m, w| m.max((w[1] - w[0]).abs()));
        assert!(max_jump < 0.15, "Reverb mode switch click: {max_jump}");
    }
}
