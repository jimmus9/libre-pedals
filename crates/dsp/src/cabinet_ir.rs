//! Simulador de Gabinete (*Cabinet IR*) por convolución particionada híbrida de **latencia cero**
//! (`Gardner 1995 / UPOLS`, decisión `D15` Opción A).
//!
//! - Bloque de cabeza (`head`, tamaño $B$) convolucionado en el dominio del tiempo directo -> **`0` muestras de latencia**.
//! - Cola restante particionada en bloques uniformes de tamaño $B$ sobre FFT de tamaño $2B$ en un FDL (*Frequency-Domain Delay Line*) prealojado sin ninguna asignación en `process()`.
//! - **3 IRs libres integradas (`GPL-3.0-or-later`)**: `1x12 Clean`, `2x12 Crunch`, `4x12 High-Gain`.
//! - Soporte de carga de archivos **`.wav` externos** precalculados fuera del hilo de audio mediante [`CustomIrAsset`] y copiados sin bloqueos ni reservas dinámicas.
//! - Filtros `Low Cut` (`20–250 Hz`) y `High Cut` (`3–18 kHz`) sobre [`SvfTpt`].

use std::path::Path;
use std::sync::Arc;

use crate::analyzer::fft_in_place;
use crate::smoother::OnePoleSmoother;
use crate::svf::{SvfMode, SvfTpt};
use crate::wav::{load_wav_file, normalize_ir_guitar_band, resample_ir_sinc};
use crate::{db_to_lin, sanitize_sample, Effect, ParamScale, ParamSpec};

const CAB_MODELS: &[&str] = &["1x12 Clean", "2x12 Crunch", "4x12 High-Gain", "Custom WAV"];

const MAX_IR_SECONDS: f32 = 0.100;
const FILTER_UPDATE_SUBBLOCK: usize = 16;

const CAB_PARAMS: [ParamSpec; 5] = [
    ParamSpec {
        id: "model",
        name: "Cabinet",
        unit: "",
        min: 0.0,
        max: 3.0,
        default: 1.0,
        scale: ParamScale::Stepped { labels: CAB_MODELS },
        decimals: 0,
    },
    ParamSpec {
        id: "low_cut_hz",
        name: "Low Cut",
        unit: "Hz",
        min: 20.0,
        max: 250.0,
        default: 75.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "high_cut_hz",
        name: "High Cut",
        unit: "Hz",
        min: 3_000.0,
        max: 18_000.0,
        default: 8_500.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "mix",
        name: "Mix",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 100.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "level_db",
        name: "Level",
        unit: "dB",
        min: -18.0,
        max: 12.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
];

/// Tasas estándar precalculadas al cargar un `.wav` externo fuera del hilo de audio.
const PRECOMPUTED_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Tamaño de partición $B$ según $f_s$ para mantener latencia `0` y carga equilibrada hasta `192 kHz`.
#[inline]
pub fn partition_size_for_rate(sample_rate: f32) -> usize {
    if sample_rate <= 50_000.0 {
        64
    } else if sample_rate <= 100_000.0 {
        128
    } else {
        256
    }
}

/// Respuesta al impulso pre-particionada (cabeza temporal + espectros FFT de cola).
#[derive(Debug, Clone)]
pub struct PreparedIrPartitions {
    pub sample_rate: u32,
    pub block_size: usize,
    pub head: Vec<f32>,
    pub tail_re: Vec<f32>,
    pub tail_im: Vec<f32>,
    pub tail_parts: usize,
}

impl PreparedIrPartitions {
    /// Convierte una IR mono ya remuestreada a `sample_rate` en particiones UPOLS (`B` y `2B`).
    pub fn from_ir(ir: &[f32], sample_rate: f32) -> Self {
        let sr_f = sample_rate.max(1.0);
        let sr_u = sr_f.round() as u32;
        let b = partition_size_for_rate(sr_f);
        let n_fft = 2 * b;
        let max_samples = ((MAX_IR_SECONDS * sr_f).ceil() as usize).max(b);
        let used_len = ir.len().min(max_samples);

        let mut head = vec![0.0_f32; b];
        let head_copy = used_len.min(b);
        for (dst, &src) in head[..head_copy].iter_mut().zip(ir[..head_copy].iter()) {
            *dst = if src.is_finite() { src } else { 0.0 };
        }

        let tail_samples = used_len.saturating_sub(b);
        let tail_parts = if tail_samples > 0 {
            tail_samples.div_ceil(b)
        } else {
            0
        };

        let mut tail_re = vec![0.0_f32; tail_parts * n_fft];
        let mut tail_im = vec![0.0_f32; tail_parts * n_fft];
        let mut work_re = vec![0.0_f32; n_fft];
        let mut work_im = vec![0.0_f32; n_fft];

        for p in 0..tail_parts {
            work_re.fill(0.0);
            work_im.fill(0.0);
            let start = (p + 1) * b;
            let end = (start + b).min(used_len);
            for (dst, &src) in work_re[..end - start].iter_mut().zip(ir[start..end].iter()) {
                *dst = if src.is_finite() { src } else { 0.0 };
            }
            fft_in_place(&mut work_re, &mut work_im);
            let dst_off = p * n_fft;
            tail_re[dst_off..dst_off + n_fft].copy_from_slice(&work_re);
            tail_im[dst_off..dst_off + n_fft].copy_from_slice(&work_im);
        }

        Self {
            sample_rate: sr_u,
            block_size: b,
            head,
            tail_re,
            tail_im,
            tail_parts,
        }
    }

    /// Remuestrea una IR desde `in_rate` hacia `out_rate`, opcionalmente normaliza en banda
    /// de guitarra (`120–3500 Hz`) y construye sus particiones UPOLS.
    pub fn from_wav_samples(samples: &[f32], in_rate: u32, out_rate: u32) -> Self {
        let out_sr_f = (out_rate as f32).max(1.0);
        let b = partition_size_for_rate(out_sr_f);
        let max_samples = ((MAX_IR_SECONDS * out_sr_f).ceil() as usize).max(b);
        let mut resampled = resample_ir_sinc(samples, in_rate, out_rate, max_samples);
        normalize_ir_guitar_band(&mut resampled, out_sr_f);
        Self::from_ir(&resampled, out_sr_f)
    }
}

/// Recurso precalculado de IR `.wav` externo listo para transferirse sin bloqueos al hilo de audio.
#[derive(Debug, Clone)]
pub struct CustomIrAsset {
    pub path: String,
    pub name: String,
    pub raw_sample_rate: u32,
    pub raw_samples: Arc<[f32]>,
    prepared_rates: Vec<(u32, PreparedIrPartitions)>,
}

impl CustomIrAsset {
    /// Carga un `.wav` desde disco, remuestrea y precalcula sus particiones para todas las $f_s$ estándar.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let wav = load_wav_file(path)?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Custom IR")
            .to_string();
        let path_str = path.to_string_lossy().to_string();
        let raw_samples: Arc<[f32]> = Arc::from(wav.samples);

        let mut prepared_rates = Vec::with_capacity(PRECOMPUTED_RATES.len());
        for &sr in &PRECOMPUTED_RATES {
            let parts = PreparedIrPartitions::from_wav_samples(&raw_samples, wav.sample_rate, sr);
            prepared_rates.push((sr, parts));
        }

        Ok(Self {
            path: path_str,
            name,
            raw_sample_rate: wav.sample_rate,
            raw_samples,
            prepared_rates,
        })
    }

    /// Devuelve las particiones precalculadas para `sample_rate` (o las calcula si es exótica).
    pub fn partitions_for_rate(&self, sample_rate: u32) -> Option<&PreparedIrPartitions> {
        self.prepared_rates
            .iter()
            .find(|(sr, _)| *sr == sample_rate)
            .map(|(_, p)| p)
    }

    /// Versión para tasas arbitrarias fuera de [`PRECOMPUTED_RATES`] (solo usar fuera del hilo de audio).
    pub fn build_partitions_for_rate(&self, sample_rate: u32) -> PreparedIrPartitions {
        if let Some(p) = self.partitions_for_rate(sample_rate) {
            return p.clone();
        }
        PreparedIrPartitions::from_wav_samples(&self.raw_samples, self.raw_sample_rate, sample_rate)
    }
}

/// Convolucionador híbrido UPOLS de **latencia cero** (`head` directo + `tail` en FDL FFT).
///
/// Toda memoria se reserva en `prepare(sample_rate)` para IRs de hasta `100 ms`.
pub struct UpolsConvolver {
    block_size: usize,
    n_fft: usize,
    max_tail_parts: usize,
    active_tail_parts: usize,

    // Cabeza directa en orden invertido [h[B-1], ..., h[0]] + doble anillo 2B para acceso contiguo.
    head_ir_rev: Vec<f32>,
    head_hist: Vec<f32>,
    head_pos: usize,

    // Particiones de cola precalculadas (FFT 2B aplanada).
    tail_ir_re: Vec<f32>,
    tail_ir_im: Vec<f32>,

    // Línea de retardo en frecuencia (FDL) circular.
    fdl_re: Vec<f32>,
    fdl_im: Vec<f32>,
    fdl_head: usize,

    // Acumulación de bloques para Overlap-Save exacto con latencia cero.
    prev_in_block: Vec<f32>,
    in_block: Vec<f32>,
    tail_out: Vec<f32>,
    block_pos: usize,

    // Trabajo temporal prealojado para FFT / IFFT.
    fft_re: Vec<f32>,
    fft_im: Vec<f32>,
    acc_re: Vec<f32>,
    acc_im: Vec<f32>,
}

impl Default for UpolsConvolver {
    fn default() -> Self {
        Self::new(48_000.0)
    }
}

impl UpolsConvolver {
    pub fn new(sample_rate: f32) -> Self {
        let mut c = Self {
            block_size: 64,
            n_fft: 128,
            max_tail_parts: 0,
            active_tail_parts: 0,
            head_ir_rev: Vec::new(),
            head_hist: Vec::new(),
            head_pos: 0,
            tail_ir_re: Vec::new(),
            tail_ir_im: Vec::new(),
            fdl_re: Vec::new(),
            fdl_im: Vec::new(),
            fdl_head: 0,
            prev_in_block: Vec::new(),
            in_block: Vec::new(),
            tail_out: Vec::new(),
            block_pos: 0,
            fft_re: Vec::new(),
            fft_im: Vec::new(),
            acc_re: Vec::new(),
            acc_im: Vec::new(),
        };
        c.prepare(sample_rate);
        c
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn max_tail_parts(&self) -> usize {
        self.max_tail_parts
    }

    pub fn prepare(&mut self, sample_rate: f32) {
        let sr = sample_rate.max(1.0);
        let b = partition_size_for_rate(sr);
        let n_fft = 2 * b;
        let max_samples = ((MAX_IR_SECONDS * sr).ceil() as usize).max(b);
        let max_parts = max_samples.div_ceil(b).max(2);
        let max_tail = max_parts - 1;

        self.block_size = b;
        self.n_fft = n_fft;
        self.max_tail_parts = max_tail;
        self.active_tail_parts = 0;

        self.head_ir_rev.resize(b, 0.0);
        self.head_ir_rev.fill(0.0);
        self.head_ir_rev[b - 1] = 1.0; // Impulso unitario inicial hasta instalar IR real.

        self.head_hist.resize(2 * b, 0.0);
        self.tail_ir_re.resize(max_tail * n_fft, 0.0);
        self.tail_ir_im.resize(max_tail * n_fft, 0.0);
        self.fdl_re.resize(max_tail * n_fft, 0.0);
        self.fdl_im.resize(max_tail * n_fft, 0.0);

        self.prev_in_block.resize(b, 0.0);
        self.in_block.resize(b, 0.0);
        self.tail_out.resize(b, 0.0);
        self.fft_re.resize(n_fft, 0.0);
        self.fft_im.resize(n_fft, 0.0);
        self.acc_re.resize(n_fft, 0.0);
        self.acc_im.resize(n_fft, 0.0);

        self.reset();
    }

    pub fn reset(&mut self) {
        self.head_hist.fill(0.0);
        self.head_pos = 0;
        self.fdl_re.fill(0.0);
        self.fdl_im.fill(0.0);
        self.fdl_head = 0;
        self.prev_in_block.fill(0.0);
        self.in_block.fill(0.0);
        self.tail_out.fill(0.0);
        self.block_pos = 0;
    }

    /// Instala particiones precalculadas mediante copias directas (`0` asignaciones en tiempo real).
    pub fn load_partitions(&mut self, parts: &PreparedIrPartitions) {
        if parts.block_size != self.block_size {
            return;
        }
        let b = self.block_size;
        let copy_head = parts.head.len().min(b);
        self.head_ir_rev.fill(0.0);
        for k in 0..copy_head {
            self.head_ir_rev[b - 1 - k] = parts.head[k];
        }

        let copy_tail = parts.tail_parts.min(self.max_tail_parts);
        let len_floats = copy_tail * self.n_fft;
        if len_floats > 0 {
            self.tail_ir_re[..len_floats].copy_from_slice(&parts.tail_re[..len_floats]);
            self.tail_ir_im[..len_floats].copy_from_slice(&parts.tail_im[..len_floats]);
        }
        self.active_tail_parts = copy_tail;
        self.reset();
    }

    /// Procesa una muestra con **latencia exacta de `0` muestras** y cero asignaciones.
    #[inline]
    pub fn process_sample(&mut self, input: f32) -> f32 {
        let b = self.block_size;
        let x = sanitize_sample(input);

        // 1. Convolución directa de cabeza sobre ventana contigua [head_pos + 1 .. head_pos + 1 + B].
        let hp = self.head_pos;
        self.head_hist[hp] = x;
        self.head_hist[hp + b] = x;

        let win = &self.head_hist[hp + 1..hp + 1 + b];
        let mut head_sum = 0.0_f32;
        for (&hx, &hi) in win.iter().zip(self.head_ir_rev.iter()) {
            head_sum += hx * hi;
        }
        self.head_pos = if hp + 1 == b { 0 } else { hp + 1 };

        // 2. Combina cabeza inmediata + cola ya calculada del bloque previo.
        let y = head_sum + self.tail_out[self.block_pos];

        // 3. Al completar un bloque de B muestras, ejecuta FFT + FDL + IFFT para el bloque siguiente.
        self.in_block[self.block_pos] = x;
        self.block_pos += 1;

        if self.block_pos == b {
            self.block_pos = 0;
            self.compute_next_tail_block();
        }

        if y.is_finite() {
            y
        } else {
            0.0
        }
    }

    fn compute_next_tail_block(&mut self) {
        let b = self.block_size;
        let n_fft = self.n_fft;

        if self.active_tail_parts == 0 || self.max_tail_parts == 0 {
            self.prev_in_block.copy_from_slice(&self.in_block);
            self.tail_out.fill(0.0);
            return;
        }

        // Overlap-Save: X_m = FFT([prev_in_block, in_block]) de longitud 2B.
        self.fft_re[..b].copy_from_slice(&self.prev_in_block);
        self.fft_re[b..n_fft].copy_from_slice(&self.in_block);
        self.fft_im.fill(0.0);
        self.prev_in_block.copy_from_slice(&self.in_block);

        fft_in_place(&mut self.fft_re, &mut self.fft_im);

        let fdl_slot = self.fdl_head * n_fft;
        self.fdl_re[fdl_slot..fdl_slot + n_fft].copy_from_slice(&self.fft_re);
        self.fdl_im[fdl_slot..fdl_slot + n_fft].copy_from_slice(&self.fft_im);

        self.acc_re.fill(0.0);
        self.acc_im.fill(0.0);

        let max_parts = self.max_tail_parts;
        for j in 0..self.active_tail_parts {
            let fdl_idx = (self.fdl_head + max_parts - j) % max_parts;
            let x_off = fdl_idx * n_fft;
            let h_off = j * n_fft;

            let xr_slice = &self.fdl_re[x_off..x_off + n_fft];
            let xi_slice = &self.fdl_im[x_off..x_off + n_fft];
            let hr_slice = &self.tail_ir_re[h_off..h_off + n_fft];
            let hi_slice = &self.tail_ir_im[h_off..h_off + n_fft];

            for k in 0..n_fft {
                let xr = xr_slice[k];
                let xi = xi_slice[k];
                let hr = hr_slice[k];
                let hi = hi_slice[k];
                self.acc_re[k] += xr * hr - xi * hi;
                self.acc_im[k] += xr * hi + xi * hr;
            }
        }

        // IFFT mediante conjugación compleja: IFFT(Z) = conj(FFT(conj(Z))) / N.
        for v in &mut self.acc_im {
            *v = -*v;
        }
        fft_in_place(&mut self.acc_re, &mut self.acc_im);
        let inv_n = 1.0 / (n_fft as f32);
        for (dst, &src) in self.tail_out.iter_mut().zip(self.acc_re[b..n_fft].iter()) {
            *dst = src * inv_n;
        }

        self.fdl_head = if self.fdl_head + 1 == max_parts {
            0
        } else {
            self.fdl_head + 1
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchState {
    Idle,
    FadingOut,
    FadingIn,
}

/// Pedal **Cabinet IR** (`UPOLS` latencia cero, 3 gabinetes integrados + carga de `.wav` externo).
pub struct CabinetIr {
    model: f32,
    low_cut_hz: f32,
    high_cut_hz: f32,
    mix: f32,
    level_db: f32,
    sample_rate: f32,

    convolver: UpolsConvolver,
    builtin_parts: [PreparedIrPartitions; 3],

    // Ranura prealojada para la IR personalizada actual (0 asignaciones al cambiar en audio).
    custom_parts: PreparedIrPartitions,
    has_custom: bool,
    loaded_custom_version: u64,

    active_model_idx: usize,
    pending_model_idx: usize,
    switch_state: SwitchState,
    switch_pos: usize,
    switch_half_len: usize,

    low_cut_svf: SvfTpt,
    high_cut_svf: SvfTpt,
    low_cut_s: OnePoleSmoother,
    high_cut_s: OnePoleSmoother,
    mix_s: OnePoleSmoother,
    level_s: OnePoleSmoother,
    sub_counter: usize,
}

impl Default for CabinetIr {
    fn default() -> Self {
        Self::new()
    }
}

impl CabinetIr {
    pub fn new() -> Self {
        let sr = 48_000.0;
        let empty_parts = PreparedIrPartitions::from_ir(&[1.0], sr);
        let mut cab = Self {
            model: CAB_PARAMS[0].default,
            low_cut_hz: CAB_PARAMS[1].default,
            high_cut_hz: CAB_PARAMS[2].default,
            mix: CAB_PARAMS[3].default,
            level_db: CAB_PARAMS[4].default,
            sample_rate: sr,
            convolver: UpolsConvolver::new(sr),
            builtin_parts: [
                empty_parts.clone(),
                empty_parts.clone(),
                empty_parts.clone(),
            ],
            custom_parts: empty_parts,
            has_custom: false,
            loaded_custom_version: 0,
            active_model_idx: 1,
            pending_model_idx: 1,
            switch_state: SwitchState::Idle,
            switch_pos: 0,
            switch_half_len: 120,
            low_cut_svf: SvfTpt::new(sr),
            high_cut_svf: SvfTpt::new(sr),
            low_cut_s: OnePoleSmoother::new(10.0, sr, CAB_PARAMS[1].default),
            high_cut_s: OnePoleSmoother::new(10.0, sr, CAB_PARAMS[2].default),
            mix_s: OnePoleSmoother::new(10.0, sr, CAB_PARAMS[3].default * 0.01),
            level_s: OnePoleSmoother::new(10.0, sr, db_to_lin(CAB_PARAMS[4].default)),
            sub_counter: 0,
        };
        cab.prepare(sr, 512);
        cab
    }

    pub fn set_model(&mut self, model: f32) {
        self.set_param(0, model);
    }

    pub fn set_low_cut_hz(&mut self, hz: f32) {
        self.set_param(1, hz);
    }

    pub fn set_high_cut_hz(&mut self, hz: f32) {
        self.set_param(2, hz);
    }

    pub fn set_mix(&mut self, pct: f32) {
        self.set_param(3, pct);
    }

    pub fn set_level_db(&mut self, db: f32) {
        self.set_param(4, db);
    }

    #[inline]
    pub fn loaded_custom_version(&self) -> u64 {
        self.loaded_custom_version
    }

    /// Instala particiones de una IR personalizada desde memoria precalculada sin asignar en audio.
    pub fn install_custom_partitions(&mut self, version: u64, parts: &PreparedIrPartitions) {
        if parts.block_size != self.convolver.block_size() {
            return;
        }
        let max_tail = self.convolver.max_tail_parts();
        let copy_head = parts.head.len().min(self.custom_parts.head.len());
        self.custom_parts.head.fill(0.0);
        self.custom_parts.head[..copy_head].copy_from_slice(&parts.head[..copy_head]);

        let copy_tail = parts.tail_parts.min(max_tail);
        let n_fft = 2 * self.custom_parts.block_size;
        let len_floats = copy_tail * n_fft;
        if len_floats > 0 && len_floats <= self.custom_parts.tail_re.len() {
            self.custom_parts.tail_re[..len_floats].copy_from_slice(&parts.tail_re[..len_floats]);
            self.custom_parts.tail_im[..len_floats].copy_from_slice(&parts.tail_im[..len_floats]);
        }
        self.custom_parts.tail_parts = copy_tail;
        self.has_custom = true;
        self.loaded_custom_version = version;

        if CAB_PARAMS[0].step_index(self.model) == 3 {
            self.trigger_model_reload(3);
        }
    }

    /// Limpia la IR personalizada volviendo de forma segura al gabinete integrado (`2x12 Crunch`).
    pub fn clear_custom_ir(&mut self, version: u64) {
        self.has_custom = false;
        self.loaded_custom_version = version;
        if CAB_PARAMS[0].step_index(self.model) == 3 {
            self.trigger_model_reload(3);
        }
    }

    /// Helper para pruebas matemáticas: carga una IR sin normalizar ni filtrar en modo `Custom WAV`.
    pub fn load_test_ir_unscaled(&mut self, ir: &[f32]) {
        let parts = PreparedIrPartitions::from_ir(ir, self.sample_rate);
        let ver = self.loaded_custom_version.saturating_add(1);
        self.install_custom_partitions(ver, &parts);
        self.model = 3.0;
        self.active_model_idx = 3;
        self.pending_model_idx = 3;
        self.switch_state = SwitchState::Idle;
        self.convolver.load_partitions(&self.custom_parts);
    }

    fn trigger_model_reload(&mut self, target_idx: usize) {
        self.pending_model_idx = target_idx.min(3);
        if self.switch_state == SwitchState::Idle {
            self.switch_state = SwitchState::FadingOut;
            self.switch_pos = 0;
        } else if self.switch_state == SwitchState::FadingIn {
            // Re-encadena fundido suave si cambia otra vez durante el fade-in.
            self.switch_state = SwitchState::FadingOut;
            self.switch_pos = self.switch_half_len.saturating_sub(self.switch_pos);
        }
    }

    fn apply_selected_partitions(&mut self, model_idx: usize) {
        let idx = model_idx.min(3);
        self.active_model_idx = idx;
        if idx == 3 && self.has_custom {
            self.convolver.load_partitions(&self.custom_parts);
        } else {
            let builtin_idx = if idx < 3 { idx } else { 1 };
            self.convolver
                .load_partitions(&self.builtin_parts[builtin_idx]);
        }
    }

    /// Genera físicamente las 3 IRs libres integradas (`1x12 Clean`, `2x12 Crunch`, `4x12 High-Gain`).
    ///
    /// Modela resonancia de caja, reflexión/emisión acústica de cono, interferencia de micro
    /// (*cone breakup / notch*) y caída natural de 4.º orden por encima de `4.5–5.5 kHz`.
    fn synthesize_builtin_ir(model_idx: usize, sample_rate: f32) -> PreparedIrPartitions {
        let sr = sample_rate.max(1.0);
        let len = match model_idx {
            0 => ((0.046 * sr).round() as usize).max(256),
            1 => ((0.052 * sr).round() as usize).max(256),
            _ => ((0.058 * sr).round() as usize).max(256),
        };
        let mut ir = vec![0.0_f32; len];

        // Impulso inicial con reflexión acústica de caja / difracción de bafle en fase temprana.
        ir[0] = 1.0;
        match model_idx {
            0 => {
                // 1x12 Open-Back Clean: ligera cancelación trasera de caja abierta y aire ambiental.
                let open_delay = ((0.0023 * sr).round() as usize).clamp(4, len - 1);
                ir[open_delay] -= 0.14;
                let room_d1 = ((0.0068 * sr).round() as usize).clamp(8, len - 1);
                ir[room_d1] += 0.055;
                let room_d2 = ((0.0145 * sr).round() as usize).clamp(12, len - 1);
                ir[room_d2] -= 0.032;
            }
            1 => {
                // 2x12 Semi-Open Crunch: doble altavoz con ligera suma constructiva/destructiva de micro.
                let spk_comb = ((0.00028 * sr).round() as usize).clamp(1, len - 1);
                ir[spk_comb] += 0.12;
                let back_d = ((0.0031 * sr).round() as usize).clamp(6, len - 1);
                ir[back_d] -= 0.11;
                let mode_d = ((0.0094 * sr).round() as usize).clamp(12, len - 1);
                ir[mode_d] += 0.048;
            }
            _ => {
                // 4x12 Closed-Back High-Gain: acoplamiento cerrado de 4 conos con presión en graves.
                let quad_d = ((0.00022 * sr).round() as usize).clamp(1, len - 1);
                ir[quad_d] += 0.18;
                let cab_ref1 = ((0.0014 * sr).round() as usize).clamp(4, len - 1);
                ir[cab_ref1] -= 0.15;
                let cab_ref2 = ((0.0042 * sr).round() as usize).clamp(8, len - 1);
                ir[cab_ref2] += 0.085;
            }
        }

        // Cadena de filtros físicos del cono + recinto en `f64` y decaimiento natural al final.
        let mut hp = SvfTpt::new(sr);
        let mut box_peak = SvfTpt::new(sr);
        let mut mid_scoop = SvfTpt::new(sr);
        let mut cone_bite = SvfTpt::new(sr);
        let mut mic_notch = SvfTpt::new(sr);
        let mut lp1 = SvfTpt::new(sr);
        let mut lp2 = SvfTpt::new(sr);

        match model_idx {
            0 => {
                // 1x12 Clean
                hp.set_params(SvfMode::HighPass, 88.0, 2.1, 0.0);
                box_peak.set_params(SvfMode::Bell, 120.0, 1.4, 3.0);
                mid_scoop.set_params(SvfMode::Bell, 620.0, 0.9, -3.0);
                cone_bite.set_params(SvfMode::Bell, 2_850.0, 1.5, 4.0);
                mic_notch.set_params(SvfMode::Bell, 4_200.0, 2.8, -5.0);
                lp1.set_params(SvfMode::LowPass, 5_300.0_f32.min(0.45 * sr), 0.707, 0.0);
                lp2.set_params(SvfMode::LowPass, 6_400.0_f32.min(0.45 * sr), 0.707, 0.0);
            }
            1 => {
                // 2x12 Crunch
                hp.set_params(SvfMode::HighPass, 102.0, 2.5, 0.0);
                box_peak.set_params(SvfMode::Bell, 138.0, 1.5, 3.5);
                mid_scoop.set_params(SvfMode::Bell, 1_150.0, 1.1, 3.5);
                cone_bite.set_params(SvfMode::Bell, 2_350.0, 1.7, 4.2);
                mic_notch.set_params(SvfMode::Bell, 3_900.0, 3.0, -7.5);
                lp1.set_params(SvfMode::LowPass, 4_850.0_f32.min(0.45 * sr), 0.707, 0.0);
                lp2.set_params(SvfMode::LowPass, 5_600.0_f32.min(0.45 * sr), 0.707, 0.0);
            }
            _ => {
                // 4x12 High-Gain
                hp.set_params(SvfMode::HighPass, 115.0, 3.0, 0.0);
                box_peak.set_params(SvfMode::Bell, 145.0, 1.7, 4.5);
                mid_scoop.set_params(SvfMode::Bell, 430.0, 1.1, -3.8);
                cone_bite.set_params(SvfMode::Bell, 2_480.0, 1.8, 5.2);
                mic_notch.set_params(SvfMode::Bell, 4_250.0, 3.2, -9.0);
                lp1.set_params(SvfMode::LowPass, 4_500.0_f32.min(0.45 * sr), 0.707, 0.0);
                lp2.set_params(SvfMode::LowPass, 5_100.0_f32.min(0.45 * sr), 0.707, 0.0);
            }
        }

        for (i, s) in ir.iter_mut().enumerate() {
            let mut y = *s as f64;
            y = hp.process_sample_f64(y);
            y = box_peak.process_sample_f64(y);
            y = mid_scoop.process_sample_f64(y);
            y = cone_bite.process_sample_f64(y);
            y = mic_notch.process_sample_f64(y);
            y = lp1.process_sample_f64(y);
            y = lp2.process_sample_f64(y);

            // Ventana de amortiguamiento suave en el último 20% para evitar truncación brusca.
            let fade_start = (len * 4) / 5;
            if i >= fade_start {
                let t = (i - fade_start) as f64 / (len - fade_start).max(1) as f64;
                let w = 0.5 * (1.0 + (std::f64::consts::PI * t).cos());
                y *= w;
            }
            *s = y as f32;
        }

        normalize_ir_guitar_band(&mut ir, sr);
        PreparedIrPartitions::from_ir(&ir, sr)
    }

    #[inline]
    fn update_cut_filters(&mut self) {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        let lc = self
            .low_cut_s
            .current()
            .clamp(15.0, 0.45 * self.sample_rate);
        let hc = self
            .high_cut_s
            .current()
            .clamp(500.0, 0.48 * self.sample_rate);
        self.low_cut_svf.set_params(SvfMode::HighPass, lc, q, 0.0);
        self.high_cut_svf.set_params(SvfMode::LowPass, hc, q, 0.0);
    }
}

impl Effect for CabinetIr {
    fn kind(&self) -> &'static str {
        "cabinet_ir"
    }

    fn name(&self) -> &'static str {
        "Cabinet IR"
    }

    fn accent_color(&self) -> [u8; 3] {
        [249, 115, 22]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &CAB_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.model,
            1 => self.low_cut_hz,
            2 => self.high_cut_hz,
            3 => self.mix,
            4 => self.level_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(spec) = CAB_PARAMS.get(index) else {
            return;
        };
        let v = spec.clamp(value);
        match index {
            0 => {
                let prev = spec.step_index(self.model);
                let next = spec.step_index(v);
                self.model = v;
                if prev != next {
                    self.trigger_model_reload(next);
                }
            }
            1 => {
                self.low_cut_hz = v;
                self.low_cut_s.set_target(v);
            }
            2 => {
                self.high_cut_hz = v;
                self.high_cut_s.set_target(v);
            }
            3 => {
                self.mix = v;
                self.mix_s.set_target(v * 0.01);
            }
            4 => {
                self.level_db = v;
                self.level_s.set_target(db_to_lin(v));
            }
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, _max_block: usize) {
        let sr = sample_rate.max(1.0);
        self.sample_rate = sr;
        self.convolver.prepare(sr);

        let b = self.convolver.block_size();
        let max_tail = self.convolver.max_tail_parts();
        let n_fft = 2 * b;
        self.custom_parts = PreparedIrPartitions {
            sample_rate: sr.round() as u32,
            block_size: b,
            head: vec![0.0; b],
            tail_re: vec![0.0; max_tail * n_fft],
            tail_im: vec![0.0; max_tail * n_fft],
            tail_parts: 0,
        };
        self.has_custom = false;
        self.loaded_custom_version = 0;

        self.builtin_parts = [
            Self::synthesize_builtin_ir(0, sr),
            Self::synthesize_builtin_ir(1, sr),
            Self::synthesize_builtin_ir(2, sr),
        ];

        self.switch_half_len = (0.0025 * sr).round().max(8.0) as usize;
        self.low_cut_svf.set_sample_rate(sr);
        self.high_cut_svf.set_sample_rate(sr);
        self.low_cut_s.set_sample_rate(sr);
        self.high_cut_s.set_sample_rate(sr);
        self.mix_s.set_sample_rate(sr);
        self.level_s.set_sample_rate(sr);

        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            let x = sanitize_sample(*s);

            let _lc_val = self.low_cut_s.next_f64();
            let _hc_val = self.high_cut_s.next_f64();
            if self.sub_counter == 0 {
                self.update_cut_filters();
            }
            self.sub_counter = (self.sub_counter + 1) % FILTER_UPDATE_SUBBLOCK;

            // Fundido corto anti-clic al cambiar de gabinete (2.5 ms out -> carga -> 2.5 ms in).
            let switch_gain = match self.switch_state {
                SwitchState::Idle => 1.0_f32,
                SwitchState::FadingOut => {
                    let g = 1.0 - (self.switch_pos as f32 / self.switch_half_len as f32);
                    self.switch_pos += 1;
                    if self.switch_pos >= self.switch_half_len {
                        let target = self.pending_model_idx;
                        self.apply_selected_partitions(target);
                        self.switch_state = SwitchState::FadingIn;
                        self.switch_pos = 0;
                    }
                    g.clamp(0.0, 1.0)
                }
                SwitchState::FadingIn => {
                    let g = self.switch_pos as f32 / self.switch_half_len as f32;
                    self.switch_pos += 1;
                    if self.switch_pos >= self.switch_half_len {
                        self.switch_state = SwitchState::Idle;
                        self.switch_pos = 0;
                    }
                    g.clamp(0.0, 1.0)
                }
            };

            let conv = self.convolver.process_sample(x) * switch_gain;
            let hp = self.low_cut_svf.process_sample_f64(conv as f64);
            let wet = self.high_cut_svf.process_sample_f64(hp) as f32;

            let mix = self.mix_s.next_val();
            let lvl = self.level_s.next_val();
            *s = (x * (1.0 - mix) + wet * mix) * lvl;
        }
    }

    fn reset(&mut self) {
        let model_idx = CAB_PARAMS[0].step_index(self.model);
        self.pending_model_idx = model_idx;
        self.switch_state = SwitchState::Idle;
        self.switch_pos = 0;

        self.low_cut_s.reset(self.low_cut_hz);
        self.high_cut_s.reset(self.high_cut_hz);
        self.mix_s.reset(self.mix * 0.01);
        self.level_s.reset(db_to_lin(self.level_db));
        self.sub_counter = 0;

        self.update_cut_filters();
        self.low_cut_svf.reset();
        self.high_cut_svf.reset();
        self.apply_selected_partitions(model_idx);
    }
}
