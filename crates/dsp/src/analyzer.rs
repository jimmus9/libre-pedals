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

/// Tipo de ventana espectral para análisis FFT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowKind {
    Hann,
    #[default]
    BlackmanHarris4,
}

/// Métricas armónicas y de aliasing calculadas sobre una ventana de análisis espectral.
#[derive(Debug, Clone, Copy, Default)]
pub struct HarmonicMetrics {
    pub fundamental_hz: f32,
    pub fundamental_db: f32,
    pub h2_db: f32,
    pub h3_db: f32,
    pub thd_percent: f32,
    pub thd_db: f32,
    pub even_odd_ratio_db: f32,
    pub even_vs_fund_db: f32,
    pub odd_vs_fund_db: f32,
    pub aliasing_snr_db: f32,
}

#[inline]
fn window_sample_and_coherent_gain(kind: WindowKind, i: usize, denom: f32) -> (f32, f32) {
    match kind {
        WindowKind::Hann => {
            let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / denom).cos());
            (w, 0.5)
        }
        WindowKind::BlackmanHarris4 => {
            let x = 2.0 * PI * i as f32 / denom;
            let w =
                0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos();
            (w, 0.35875)
        }
    }
}

fn compute_fft_magnitudes(samples: &[f32], window: WindowKind) -> Vec<f32> {
    let n = samples.len();
    if n < 2 || !n.is_power_of_two() {
        return Vec::new();
    }
    let mut re = vec![0.0_f32; n];
    let mut im = vec![0.0_f32; n];
    let denom = (n - 1).max(1) as f32;
    let mut coherent_gain = 0.5_f32;

    for (i, &x) in samples.iter().enumerate() {
        let (w, cg) = window_sample_and_coherent_gain(window, i, denom);
        coherent_gain = cg;
        re[i] = sanitize_sample(x) * w;
    }

    fft_in_place(&mut re, &mut im);

    let norm = (n as f32) * 0.5 * coherent_gain;
    let bins = n / 2;
    let mut mags = Vec::with_capacity(bins);
    for k in 0..bins {
        let mag = (re[k] * re[k] + im[k] * im[k]).sqrt() / norm;
        mags.push(if mag.is_finite() { mag } else { 0.0 });
    }
    mags
}

/// Calcula el espectro en dBFS usando una ventana seleccionable (`Hann` o `BlackmanHarris4`).
/// Escribe `samples.len() / 2` bins en `out_mag_db`, con piso de `-120.0 dB` (acotado al rango pedido).
pub fn compute_spectrum_db_with_window(
    samples: &[f32],
    out_mag_db: &mut [f32],
    window: WindowKind,
) {
    let n = samples.len();
    if n < 2 || !n.is_power_of_two() || out_mag_db.is_empty() {
        return;
    }

    let mags = compute_fft_magnitudes(samples, window);
    let floor_db = match window {
        WindowKind::Hann => -100.0_f32,
        WindowKind::BlackmanHarris4 => -120.0_f32,
    };
    let floor_lin = 10.0_f32.powf(floor_db * 0.05);

    let bins = (n / 2).min(out_mag_db.len());
    for k in 0..bins {
        let mag = mags[k];
        out_mag_db[k] = if mag <= floor_lin || !mag.is_finite() {
            floor_db
        } else {
            (20.0 * mag.log10()).max(floor_db)
        };
    }
}

/// Calcula la magnitud del espectro en dBFS usando ventana Hann sobre `samples` (potencia de 2).
/// Escribe `samples.len() / 2` bins en `out_mag_db`, con piso de `-100.0 dB`.
pub fn compute_spectrum_db(samples: &[f32], out_mag_db: &mut [f32]) {
    compute_spectrum_db_with_window(samples, out_mag_db, WindowKind::Hann);
}

/// Genera una senoidal coherente (número entero de ciclos sobre `num_samples`) y devuelve `(muestras, f_exacta_hz)`.
pub fn generate_coherent_sine(
    approx_freq_hz: f32,
    sample_rate: f32,
    num_samples: usize,
    amplitude: f32,
) -> (Vec<f32>, f32) {
    let sr = sample_rate.max(1.0);
    let n = num_samples.max(2);
    let cycles = ((approx_freq_hz * n as f32) / sr).round().max(1.0);
    let exact_freq = cycles * sr / n as f32;
    let phase_inc = 2.0 * PI * cycles / n as f32;
    let out = (0..n)
        .map(|i| amplitude * (phase_inc * i as f32).sin())
        .collect();
    (out, exact_freq)
}

#[inline]
fn band_power_around(mags: &[f32], center_bin: usize, half_width: usize) -> f64 {
    if mags.is_empty() || center_bin >= mags.len() {
        return 0.0;
    }
    let lo = center_bin.saturating_sub(half_width).max(1);
    let hi = (center_bin + half_width).min(mags.len() - 1);
    let mut sum = 0.0_f64;
    for &m in &mags[lo..=hi] {
        let mf = m as f64;
        sum += mf * mf;
    }
    sum
}

/// Calcula fundamental, H2, H3, THD%, balance pares/impares y relación señal-a-aliasing/ruido (`ASNR`).
pub fn compute_harmonic_metrics(
    samples: &[f32],
    sample_rate: f32,
    window: WindowKind,
    known_fundamental_hz: Option<f32>,
) -> HarmonicMetrics {
    let n = samples.len();
    if n < 32 || !n.is_power_of_two() || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return HarmonicMetrics::default();
    }

    let mags = compute_fft_magnitudes(samples, window);
    let bins = mags.len();
    let bin_hz = sample_rate / n as f32;
    let half_w = match window {
        WindowKind::Hann => 1,
        WindowKind::BlackmanHarris4 => 2,
    };

    let k_min = (35.0 / bin_hz).ceil().max(2.0) as usize;
    let k_max = ((0.46 * sample_rate) / bin_hz).floor() as usize;
    if k_min >= bins || k_max <= k_min {
        return HarmonicMetrics::default();
    }

    let valid_known_f0 =
        known_fundamental_hz.filter(|f| f.is_finite() && *f > 0.0 && *f < 0.48 * sample_rate);

    let fund_bin = if let Some(f0) = valid_known_f0 {
        let approx = ((f0 / bin_hz).round() as usize).clamp(k_min, bins - 1);
        let lo = approx.saturating_sub(2).max(k_min);
        let hi = (approx + 2).min(bins - 1);
        (lo..=hi)
            .max_by(|&a, &b| {
                mags[a]
                    .partial_cmp(&mags[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(approx)
    } else {
        (k_min..=k_max.min(bins - 1))
            .max_by(|&a, &b| {
                mags[a]
                    .partial_cmp(&mags[b])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(k_min)
    };

    let fund_power = band_power_around(&mags, fund_bin, half_w);
    if fund_power <= 1e-12 {
        return HarmonicMetrics {
            fundamental_db: -120.0,
            h2_db: -120.0,
            h3_db: -120.0,
            thd_db: -120.0,
            even_vs_fund_db: -120.0,
            odd_vs_fund_db: -120.0,
            ..HarmonicMetrics::default()
        };
    }

    let fund_hz = valid_known_f0.unwrap_or(fund_bin as f32 * bin_hz);
    let fund_db = 10.0 * fund_power.log10() as f32;

    let mut is_harmonic_bin = vec![false; bins];
    let f_lo = fund_bin.saturating_sub(half_w + 1);
    let f_hi = (fund_bin + half_w + 1).min(bins - 1);
    is_harmonic_bin[f_lo..=f_hi].fill(true);

    let mut h2_db = -120.0_f32;
    let mut h3_db = -120.0_f32;
    let mut even_power = 0.0_f64;
    let mut odd_power = 0.0_f64;

    for m in 2..=32_usize {
        let hm_hz = fund_hz * m as f32;
        if hm_hz >= 0.48 * sample_rate {
            break;
        }
        let hb = (hm_hz / bin_hz).round() as usize;
        if hb >= bins {
            break;
        }
        let p = band_power_around(&mags, hb, half_w);
        let h_lo = hb.saturating_sub(half_w + 1);
        let h_hi = (hb + half_w + 1).min(bins - 1);
        is_harmonic_bin[h_lo..=h_hi].fill(true);
        let rel_db = (10.0 * (p / fund_power).max(1e-12).log10()) as f32;
        if m == 2 {
            h2_db = rel_db;
        } else if m == 3 {
            h3_db = rel_db;
        }
        if m % 2 == 0 {
            even_power += p;
        } else {
            odd_power += p;
        }
    }

    let harm_power = even_power + odd_power;
    let thd_ratio = (harm_power / fund_power).sqrt();
    let thd_percent = (thd_ratio * 100.0) as f32;
    let thd_db = (10.0 * (harm_power / fund_power).max(1e-12).log10()) as f32;

    let even_vs_fund_db = (10.0 * (even_power / fund_power).max(1e-12).log10()) as f32;
    let odd_vs_fund_db = (10.0 * (odd_power / fund_power).max(1e-12).log10()) as f32;
    let even_odd_ratio_db = even_vs_fund_db - odd_vs_fund_db;

    let mut nonharm_power = 0.0_f64;
    let k_noise_start = (60.0 / bin_hz).ceil().max(2.0) as usize;
    let k_noise_end = ((0.48 * sample_rate) / bin_hz).floor() as usize;
    for k in k_noise_start..=k_noise_end.min(bins - 1) {
        if !is_harmonic_bin[k] {
            let mf = mags[k] as f64;
            nonharm_power += mf * mf;
        }
    }

    let total_sig_power = fund_power + harm_power;
    let aliasing_snr_db = (10.0
        * (total_sig_power / nonharm_power.max(1e-12))
            .min(1e12)
            .log10()) as f32;

    HarmonicMetrics {
        fundamental_hz: fund_hz,
        fundamental_db: fund_db,
        h2_db,
        h3_db,
        thd_percent,
        thd_db,
        even_odd_ratio_db,
        even_vs_fund_db,
        odd_vs_fund_db,
        aliasing_snr_db,
    }
}

/// Mide la distorsión de intermodulación (`IMD` en dB respecto a la suma de portadoras `f1` y `f2`).
pub fn compute_imd_db(samples: &[f32], sample_rate: f32, f1_hz: f32, f2_hz: f32) -> f32 {
    let n = samples.len();
    if n < 32 || !n.is_power_of_two() || sample_rate <= 0.0 {
        return -120.0;
    }
    let mags = compute_fft_magnitudes(samples, WindowKind::BlackmanHarris4);
    let bin_hz = sample_rate / n as f32;
    let bin_of = |f: f32| -> usize { ((f / bin_hz).round() as usize).min(mags.len() - 1) };

    let p1 = band_power_around(&mags, bin_of(f1_hz), 2);
    let p2 = band_power_around(&mags, bin_of(f2_hz), 2);
    let carrier_power = (p1 + p2).max(1e-12);

    let products = [
        (f2_hz - f1_hz).abs(),
        f1_hz + f2_hz,
        (2.0 * f1_hz - f2_hz).abs(),
        (2.0 * f2_hz - f1_hz).abs(),
    ];
    let mut imd_power = 0.0_f64;
    for fp in products {
        if fp > 25.0 && fp < 0.48 * sample_rate {
            imd_power += band_power_around(&mags, bin_of(fp), 2);
        }
    }
    (10.0 * (imd_power / carrier_power).max(1e-12).log10()) as f32
}

/// Calcula el *Error-to-Signal Ratio* (`ESR`) en decibelios entre una señal de referencia y una candidata.
pub fn compute_esr_db(reference: &[f32], candidate: &[f32]) -> f32 {
    let len = reference.len().min(candidate.len());
    if len == 0 {
        return 0.0;
    }
    let mut err_sq = 0.0_f64;
    let mut ref_sq = 0.0_f64;
    for i in 0..len {
        let r = sanitize_sample(reference[i]) as f64;
        let c = sanitize_sample(candidate[i]) as f64;
        let d = r - c;
        err_sq += d * d;
        ref_sq += r * r;
    }
    if ref_sq <= 1e-18 {
        return if err_sq <= 1e-18 { -140.0 } else { 0.0 };
    }
    (10.0 * (err_sq / ref_sq).max(1e-14).log10()) as f32
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_harmonic_metrics_invalid_known_fundamental_falls_back_to_auto() {
        let sr = 48_000.0_f32;
        let n = 4096usize;
        let bin_hz = sr / n as f32;
        let (sine, f_exact) = generate_coherent_sine(4_370.0, sr, n, 0.7);

        let auto = compute_harmonic_metrics(&sine, sr, WindowKind::BlackmanHarris4, None);
        assert!(
            auto.fundamental_db > -20.0,
            "auto fundamental_db unexpectedly low: {}",
            auto.fundamental_db
        );
        assert!(
            (auto.fundamental_hz - f_exact).abs() < bin_hz,
            "auto fundamental_hz={} far from expected {f_exact}",
            auto.fundamental_hz
        );

        let exact = compute_harmonic_metrics(&sine, sr, WindowKind::BlackmanHarris4, Some(f_exact));
        assert!(
            (exact.fundamental_hz - f_exact).abs() < 1e-3,
            "exact fundamental_hz={} != {f_exact}",
            exact.fundamental_hz
        );
        assert!(exact.fundamental_db > -20.0);

        for invalid_f0 in [
            f32::NAN,
            0.0,
            -440.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
            100_000.0,
        ] {
            let m =
                compute_harmonic_metrics(&sine, sr, WindowKind::BlackmanHarris4, Some(invalid_f0));
            assert!(
                m.fundamental_db > -20.0,
                "invalid known_fundamental_hz={invalid_f0} produced floor fundamental_db={}",
                m.fundamental_db
            );
            assert!(
                (m.fundamental_hz - auto.fundamental_hz).abs() < 1e-4,
                "invalid known_fundamental_hz={invalid_f0} fundamental_hz={} != auto={}",
                m.fundamental_hz,
                auto.fundamental_hz
            );
            assert!(
                (m.fundamental_db - auto.fundamental_db).abs() < 1e-4,
                "invalid known_fundamental_hz={invalid_f0} fundamental_db={} != auto={}",
                m.fundamental_db,
                auto.fundamental_db
            );
            assert!(
                (m.aliasing_snr_db - auto.aliasing_snr_db).abs() < 1e-4,
                "invalid known_fundamental_hz={invalid_f0} aliasing_snr_db={} != auto={}",
                m.aliasing_snr_db,
                auto.aliasing_snr_db
            );
        }
    }
}
