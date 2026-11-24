//! Sobremuestreo elíptico *half-band* polifásico estilo **HIIR** (Laurent de Soras) con factor
//! adaptativo continuo según la frecuencia de muestreo del motor:
//!
//! - `fs < 40 kHz`  -> `8x`
//! - `40..72 kHz`   -> `4x` (44.1 y 48 kHz -> ~176–192 kHz internos)
//! - `72..144 kHz`  -> `2x` (88.2 y 96 kHz -> ~176–192 kHz internos)
//! - `fs >= 144 kHz` -> `1x` (176.4 y 192 kHz operan directo sin etapas extra)
//!
//! Cada etapa `2x` mantiene estados completamente separados para sobremuestreo y submuestreo, y
//! realiza toda la aritmética en `f64` sin asignaciones de memoria durante el procesado.

use std::f64::consts::PI;

use crate::denormals::flush_denormal_f64;

/// Determina el factor de sobremuestreo adaptativo continuo para una `sample_rate` dada.
#[inline]
pub fn adaptive_oversampling_factor(sample_rate: f32) -> usize {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return 4;
    }
    if sample_rate < 40_000.0 {
        8
    } else if sample_rate < 72_000.0 {
        4
    } else if sample_rate < 144_000.0 {
        2
    } else {
        1
    }
}

/// Coeficientes de filtro *half-band* elíptico polifásico de Laurent de Soras / Gazsi (1985).
///
/// Devuelve un vector de longitud `num_coefs` ordenado ascendentemente (`0 < c_0 < ... < 1`).
/// Las ramas `A0` (pares) y `A1` (impares) toman índices pares e impares respectivamente.
pub fn compute_hiir_coefs(num_coefs: usize, transition_bw: f64) -> Vec<f64> {
    assert!(num_coefs >= 1, "num_coefs debe ser >= 1");
    let tbw = transition_bw.clamp(0.005, 0.45);
    let fp = 0.25 - 0.5 * tbw;
    let omega_p = (PI * fp).tan();
    let k = (omega_p * omega_p).clamp(1e-9, 1.0 - 1e-9);
    let k_prime = (1.0 - k * k).sqrt();
    let u0 = 0.5 * (1.0 - k_prime.sqrt()) / (1.0 + k_prime.sqrt());
    let u0_4 = u0.powi(4);
    let q = u0 + 2.0 * u0 * u0_4 + 15.0 * u0 * u0_4 * u0_4 + 150.0 * u0 * u0_4 * u0_4 * u0_4;

    let order = 2 * num_coefs + 1;
    let q_025 = q.powf(0.25);

    let mut coefs = Vec::with_capacity(num_coefs);
    for i in 1..=num_coefs {
        let theta = PI * (i as f64) / (order as f64);
        let mut num_sum = 0.0;
        for m in 0..=4_i32 {
            let sign = if m % 2 == 0 { 1.0 } else { -1.0 };
            let exp_q = (m * (m + 1)) as f64;
            num_sum += sign * q.powf(exp_q) * ((2 * m + 1) as f64 * theta).sin();
        }
        let mut den_sum = 1.0;
        for m in 1..=5_i32 {
            let sign = if m % 2 == 0 { 1.0 } else { -1.0 };
            let exp_q = (m * m) as f64;
            den_sum += 2.0 * sign * q.powf(exp_q) * ((2 * m) as f64 * theta).cos();
        }
        let lam = 2.0 * q_025 * num_sum / den_sum;
        let lam_sq = lam * lam;
        let sigma_sq = ((1.0 - k * lam_sq) * (1.0 - lam_sq / k)).max(0.0);
        let b = sigma_sq.sqrt();
        // Mapeo LWDF birecíproco (Gazsi 1985): cada sección all-pass 1.er orden en z^-2 tiene coeficiente c_i = (1 - y) / (1 + y).
        let y = (b / (1.0 + lam_sq)).clamp(1e-9, 1.0 - 1e-9);
        let c = (1.0 - y) / (1.0 + y);
        coefs.push(c);
    }

    coefs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    coefs
}

const MAX_BRANCH_STAGES: usize = 6;

#[derive(Debug, Clone)]
struct AllPassChain {
    coefs: [f64; MAX_BRANCH_STAGES],
    x_hist: [f64; MAX_BRANCH_STAGES],
    y_hist: [f64; MAX_BRANCH_STAGES],
    len: usize,
}

impl AllPassChain {
    fn new(branch_coefs: &[f64]) -> Self {
        assert!(branch_coefs.len() <= MAX_BRANCH_STAGES);
        let mut coefs = [0.0; MAX_BRANCH_STAGES];
        for (dst, &src) in coefs.iter_mut().zip(branch_coefs.iter()) {
            *dst = src;
        }
        Self {
            coefs,
            x_hist: [0.0; MAX_BRANCH_STAGES],
            y_hist: [0.0; MAX_BRANCH_STAGES],
            len: branch_coefs.len(),
        }
    }

    fn reset(&mut self) {
        self.x_hist.fill(0.0);
        self.y_hist.fill(0.0);
    }

    /// Evalúa la cascada de filtros all-pass de 1.er orden: H_i(z) = (c_i + z^-1) / (1 + c_i z^-1).
    #[inline]
    fn process_sample(&mut self, mut x: f64) -> f64 {
        for i in 0..self.len {
            let c = self.coefs[i];
            let y = c * (x - self.y_hist[i]) + self.x_hist[i];
            self.x_hist[i] = x;
            self.y_hist[i] = flush_denormal_f64(y);
            x = self.y_hist[i];
        }
        x
    }
}

/// Etapa `2x` half-band polifásica HIIR con rutas separadas de *upsampling* y *downsampling*.
#[derive(Debug, Clone)]
pub struct HalfBandStage2x {
    up_a: AllPassChain,
    up_b: AllPassChain,
    down_a: AllPassChain,
    down_b: AllPassChain,
}

impl HalfBandStage2x {
    /// Construye la etapa `2x` a partir de coeficientes HIIR calculados para un ancho de transición dado.
    pub fn from_spec(num_coefs: usize, transition_bw: f64) -> Self {
        let coefs = compute_hiir_coefs(num_coefs, transition_bw);
        Self::from_coefs(&coefs)
    }

    /// Construye la etapa `2x` desde una lista ordenada de coeficientes all-pass.
    pub fn from_coefs(coefs: &[f64]) -> Self {
        let mut even = Vec::new();
        let mut odd = Vec::new();
        for (i, &c) in coefs.iter().enumerate() {
            if i % 2 == 0 {
                even.push(c);
            } else {
                odd.push(c);
            }
        }
        Self {
            up_a: AllPassChain::new(&even),
            up_b: AllPassChain::new(&odd),
            down_a: AllPassChain::new(&even),
            down_b: AllPassChain::new(&odd),
        }
    }

    /// Reinicia el estado interno tanto del camino de subida como del camino de bajada.
    pub fn reset(&mut self) {
        self.up_a.reset();
        self.up_b.reset();
        self.down_a.reset();
        self.down_b.reset();
    }

    /// Sube una muestra `1x -> (even, odd)` a frecuencia doble manteniendo ganancia unitaria en banda de paso.
    #[inline]
    pub fn upsample_sample(&mut self, x: f64) -> (f64, f64) {
        let y_even = self.up_a.process_sample(x);
        let y_odd = self.up_b.process_sample(x);
        (y_even, y_odd)
    }

    /// Baja un par `(even, odd)` a `1x` filtrando todo el contenido por encima de la Nyquist base.
    #[inline]
    pub fn downsample_pair(&mut self, even: f64, odd: f64) -> f64 {
        0.5 * (self.down_a.process_sample(odd) + self.down_b.process_sample(even))
    }

    /// Sube un bloque completo de `src.len()` muestras a `dst` (longitud `2 * src.len()`).
    pub fn upsample_block(&mut self, src: &[f64], dst: &mut [f64]) {
        debug_assert!(dst.len() >= src.len() * 2);
        for (i, &x) in src.iter().enumerate() {
            let (e, o) = self.upsample_sample(x);
            dst[2 * i] = e;
            dst[2 * i + 1] = o;
        }
    }

    /// Baja un bloque de `src` (longitud `2 * dst.len()`) hacia `dst`.
    pub fn downsample_block(&mut self, src: &[f64], dst: &mut [f64]) {
        debug_assert!(src.len() >= dst.len() * 2);
        for (i, out) in dst.iter_mut().enumerate() {
            *out = self.downsample_pair(src[2 * i], src[2 * i + 1]);
        }
    }

    /// Evalúa la magnitud lineal exacta de la respuesta pasa-bajos a frecuencia normalizada `norm_freq` (`0..0.5` respecto a la tasa alta).
    pub fn magnitude_response_at(&self, norm_freq: f64) -> f64 {
        let w = 2.0 * PI * norm_freq.clamp(0.0, 0.5);
        let eval_chain = |chain: &AllPassChain, w2: f64| -> (f64, f64) {
            let c2 = w2.cos();
            let s2 = w2.sin();
            let mut re = 1.0_f64;
            let mut im = 0.0_f64;
            for &c in &chain.coefs[..chain.len] {
                // (c + z^-2) / (1 + c z^-2), z^-2 = c2 - j s2
                let n_re = c + c2;
                let n_im = -s2;
                let d_re = 1.0 + c * c2;
                let d_im = -c * s2;
                let d2 = d_re * d_re + d_im * d_im;
                let s_re = (n_re * d_re + n_im * d_im) / d2;
                let s_im = (n_im * d_re - n_re * d_im) / d2;
                let nr = re * s_re - im * s_im;
                let ni = re * s_im + im * s_re;
                re = nr;
                im = ni;
            }
            (re, im)
        };
        let (a0_re, a0_im) = eval_chain(&self.down_a, 2.0 * w);
        let (a1_re, a1_im) = eval_chain(&self.down_b, 2.0 * w);
        // Multiplicamos A1 por z^-1 = cos(w) - j sin(w).
        let cw = w.cos();
        let sw = w.sin();
        let b1_re = a1_re * cw + a1_im * sw;
        let b1_im = a1_im * cw - a1_re * sw;
        let tot_re = 0.5 * (a0_re + b1_re);
        let tot_im = 0.5 * (a0_im + b1_im);
        (tot_re * tot_re + tot_im * tot_im).sqrt()
    }
}

/// Banco de sobremuestreo en cascada (`1x`, `2x`, `4x` u `8x`) preasignado en `prepare()`.
pub struct Oversampler {
    factor: usize,
    sample_rate: f32,
    stage1: HalfBandStage2x,
    stage2: HalfBandStage2x,
    stage3: HalfBandStage2x,
    buf_a: Vec<f64>,
    buf_b: Vec<f64>,
}

impl Oversampler {
    /// Crea el sobremuestreador para `sample_rate` y bloques de hasta `max_block_size` muestras.
    pub fn new(sample_rate: f32, max_block_size: usize) -> Self {
        let mut os = Self {
            factor: 1,
            sample_rate: 48_000.0,
            // Etapa interna más estrecha (~22.6 kHz paso a 48 kHz base -> ~95+ dB rechazo).
            stage1: HalfBandStage2x::from_spec(8, 0.045),
            // Etapa intermedia (2x -> 4x) con transición amplia para mínima CPU.
            stage2: HalfBandStage2x::from_spec(4, 0.25),
            // Etapa externa (4x -> 8x) ultraligera.
            stage3: HalfBandStage2x::from_spec(3, 0.35),
            buf_a: Vec::new(),
            buf_b: Vec::new(),
        };
        os.prepare(sample_rate, max_block_size);
        os
    }

    /// Configura el factor adaptativo `M(fs)` y preasigna buffers hasta `8 * max_block_size`.
    pub fn prepare(&mut self, sample_rate: f32, max_block_size: usize) {
        self.sample_rate = if sample_rate.is_finite() && sample_rate > 0.0 {
            sample_rate
        } else {
            48_000.0
        };
        self.factor = adaptive_oversampling_factor(self.sample_rate);
        let cap = max_block_size.max(1) * 8;
        if self.buf_a.len() < cap {
            self.buf_a.resize(cap, 0.0);
        }
        if self.buf_b.len() < cap {
            self.buf_b.resize(cap, 0.0);
        }
        self.reset();
    }

    /// Borra el estado interno de todas las etapas sin reasignar memoria.
    pub fn reset(&mut self) {
        self.stage1.reset();
        self.stage2.reset();
        self.stage3.reset();
    }

    /// Factor de sobremuestreo activo (`1`, `2`, `4` u `8`).
    #[inline]
    pub fn factor(&self) -> usize {
        self.factor
    }

    /// Tasa de muestreo efectiva interna (`sample_rate * factor`).
    #[inline]
    pub fn oversampled_sample_rate(&self) -> f32 {
        self.sample_rate * self.factor as f32
    }

    /// Ejecuta un núcleo no lineal `kernel(&mut [f64], oversampled_sr)` sobre el bloque en tiempo real.
    ///
    /// No realiza ninguna reserva en heap si `block.len() <= max_block_size` configurado en `prepare()`.
    pub fn process_block<F>(&mut self, block: &mut [f32], mut kernel: F)
    where
        F: FnMut(&mut [f64], f32),
    {
        let n = block.len();
        if n == 0 {
            return;
        }
        let needed = n * self.factor;
        if self.buf_a.len() < needed {
            self.buf_a.resize(needed, 0.0);
        }
        if self.buf_b.len() < needed {
            self.buf_b.resize(needed, 0.0);
        }

        let os_sr = self.oversampled_sample_rate();
        match self.factor {
            1 => {
                let work = &mut self.buf_a[..n];
                for (dst, &src) in work.iter_mut().zip(block.iter()) {
                    *dst = if src.is_finite() { src as f64 } else { 0.0 };
                }
                kernel(work, os_sr);
                for (dst, &src) in block.iter_mut().zip(work.iter()) {
                    *dst = src as f32;
                }
            }
            2 => {
                for (dst, &src) in self.buf_b[..n].iter_mut().zip(block.iter()) {
                    *dst = if src.is_finite() { src as f64 } else { 0.0 };
                }
                self.stage1
                    .upsample_block(&self.buf_b[..n], &mut self.buf_a[..2 * n]);
                kernel(&mut self.buf_a[..2 * n], os_sr);
                self.stage1
                    .downsample_block(&self.buf_a[..2 * n], &mut self.buf_b[..n]);
                for (dst, &src) in block.iter_mut().zip(self.buf_b[..n].iter()) {
                    *dst = src as f32;
                }
            }
            4 => {
                for (dst, &src) in self.buf_a[..n].iter_mut().zip(block.iter()) {
                    *dst = if src.is_finite() { src as f64 } else { 0.0 };
                }
                self.stage1
                    .upsample_block(&self.buf_a[..n], &mut self.buf_b[..2 * n]);
                self.stage2
                    .upsample_block(&self.buf_b[..2 * n], &mut self.buf_a[..4 * n]);
                kernel(&mut self.buf_a[..4 * n], os_sr);
                self.stage2
                    .downsample_block(&self.buf_a[..4 * n], &mut self.buf_b[..2 * n]);
                self.stage1
                    .downsample_block(&self.buf_b[..2 * n], &mut self.buf_a[..n]);
                for (dst, &src) in block.iter_mut().zip(self.buf_a[..n].iter()) {
                    *dst = src as f32;
                }
            }
            _ => {
                // 8x
                for (dst, &src) in self.buf_b[..n].iter_mut().zip(block.iter()) {
                    *dst = if src.is_finite() { src as f64 } else { 0.0 };
                }
                self.stage1
                    .upsample_block(&self.buf_b[..n], &mut self.buf_a[..2 * n]);
                self.stage2
                    .upsample_block(&self.buf_a[..2 * n], &mut self.buf_b[..4 * n]);
                self.stage3
                    .upsample_block(&self.buf_b[..4 * n], &mut self.buf_a[..8 * n]);
                kernel(&mut self.buf_a[..8 * n], os_sr);
                self.stage3
                    .downsample_block(&self.buf_a[..8 * n], &mut self.buf_b[..4 * n]);
                self.stage2
                    .downsample_block(&self.buf_b[..4 * n], &mut self.buf_a[..2 * n]);
                self.stage1
                    .downsample_block(&self.buf_a[..2 * n], &mut self.buf_b[..n]);
                for (dst, &src) in block.iter_mut().zip(self.buf_b[..n].iter()) {
                    *dst = src as f32;
                }
            }
        }
    }
}
