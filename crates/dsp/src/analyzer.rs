use std::f32::consts::PI;

use crate::{lin_to_db, sanitize_sample};

/// Métricas matemáticas de una ventana de forma de onda.
#[derive(Debug, Clone, Copy, Default)]
pub struct WaveformMetrics {
    pub peak: f32,
    pub rms: f32,
    pub peak_db: f32,
    pub rms_db: f32,
    /// Factor de cresta en dB (`20 * log10(peak / rms)`).
    /// Senoidal pura ≈ 3.01 dB; onda cuadrada ≈ 0.0 dB.
    pub crest_factor_db: f32,
}

/// Transformada Rápida de Fourier (Cooley-Tukey Radix-2) compleja in-place.
///
/// Requiere que `re.len() == im.len()` y que la longitud sea una potencia de 2 >= 2.
pub fn fft_in_place(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    if n < 2 || im.len() != n || !n.is_power_of_two() {
        return;
    }

    // Permutación por inversión de bits
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    // Mariposas iterativas
    let mut len = 2usize;
    while len <= n {
        let ang = -2.0 * PI / (len as f32);
        let (wlen_sin, wlen_cos) = ang.sin_cos();
        let half = len / 2;

        for start in (0..n).step_by(len) {
            let mut w_re = 1.0_f32;
            let mut w_im = 0.0_f32;
            for k in 0..half {
                let i0 = start + k;
                let i1 = i0 + half;
                let u_re = re[i0];
                let u_im = im[i0];
                let v_re = re[i1] * w_re - im[i1] * w_im;
                let v_im = re[i1] * w_im + im[i1] * w_re;
                re[i0] = u_re + v_re;
                im[i0] = u_im + v_im;
                re[i1] = u_re - v_re;
                im[i1] = u_im - v_im;

                let next_w_re = w_re * wlen_cos - w_im * wlen_sin;
                w_im = w_re * wlen_sin + w_im * wlen_cos;
                w_re = next_w_re;
            }
        }
        len <<= 1;
    }
}

/// Calcula la magnitud del espectro en dBFS usando ventana Hann sobre `samples` (potencia de 2).
/// Escribe `samples.len() / 2` bins en `out_mag_db`, con piso de `-100.0 dB`.
pub fn compute_spectrum_db(samples: &[f32], out_mag_db: &mut [f32]) {
    let n = samples.len();
    if n < 2 || !n.is_power_of_two() || out_mag_db.is_empty() {
        return;
    }

    let mut re = vec![0.0_f32; n];
    let mut im = vec![0.0_f32; n];
    let denom = (n - 1) as f32;

    for (i, &x) in samples.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / denom).cos());
        re[i] = sanitize_sample(x) * w;
    }

    fft_in_place(&mut re, &mut im);

    let norm = (n as f32) * 0.25;
    let bins = (n / 2).min(out_mag_db.len());
    for k in 0..bins {
        let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() / norm;
        let db = if mag <= 1e-5 || !mag.is_finite() {
            -100.0
        } else {
            (20.0 * mag.log10()).max(-100.0)
        };
        out_mag_db[k] = db;
    }
}

/// Calcula pico, RMS y factor de cresta sobre una ventana de muestras.
pub fn compute_waveform_metrics(samples: &[f32]) -> WaveformMetrics {
    if samples.is_empty() {
        return WaveformMetrics {
            peak: 0.0,
            rms: 0.0,
            peak_db: -120.0,
            rms_db: -120.0,
            crest_factor_db: 0.0,
        };
    }

    let mut peak = 0.0_f32;
    let mut sum_sq = 0.0_f64;
    for &raw in samples {
        let x = sanitize_sample(raw);
        let abs = x.abs();
        if abs > peak {
            peak = abs;
        }
        sum_sq += (x as f64) * (x as f64);
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;
    let crest_factor_db = if rms > 1e-6 && peak > 1e-6 {
        (20.0 * (peak / rms).log10()).clamp(0.0, 40.0)
    } else {
        0.0
    };

    WaveformMetrics {
        peak,
        rms,
        peak_db: lin_to_db(peak),
        rms_db: lin_to_db(rms),
        crest_factor_db,
    }
}

/// Busca un cruce por cero ascendente con histéresis para estabilizar el disparo del osciloscopio.
pub fn find_rising_zero_crossing(samples: &[f32], search_len: usize) -> usize {
    let limit = search_len.min(samples.len());
    if limit < 2 {
        return 0;
    }

    let mut peak = 0.0_f32;
    for &s in &samples[..limit] {
        let abs = sanitize_sample(s).abs();
        if abs > peak {
            peak = abs;
        }
    }
    if peak < 1e-5 {
        return 0;
    }

    let arm_thresh = -0.02 * peak;
    let mut armed = false;
    for (i, &raw) in samples[..limit].iter().enumerate() {
        let x = sanitize_sample(raw);
        if x < arm_thresh {
            armed = true;
        } else if armed && x >= 0.0 {
            return i;
        }
    }

    for i in 1..limit {
        if samples[i - 1] < 0.0 && samples[i] >= 0.0 {
            return i;
        }
    }

    0
}
