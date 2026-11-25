//! Lector RIFF/WAVE en Rust puro, remuestreador *sinc* offline y normalización espectral de IRs.
//!
//! Diseñado para cargar archivos `.wav` de respuestas al impulso (`Cabinet IR`) sin dependencias
//! externas (`crates/dsp` con `0` dependencias), soportando PCM `8/16/24/32` bits, IEEE Float
//! `32/64` bits y `WAVE_FORMAT_EXTENSIBLE` en cualquier tasa de muestreo estándar.

use std::f64::consts::PI;
use std::path::Path;

use crate::analyzer::fft_in_place;

/// Contenido decodificado y mezclado a mono de un archivo `.wav`.
#[derive(Debug, Clone)]
pub struct WavIrData {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Lee y decodifica un archivo `.wav` desde disco.
pub fn load_wav_file(path: &Path) -> Result<WavIrData, String> {
    let bytes = std::fs::read(path)
        .map_err(|e| format!("No se pudo leer el archivo WAV '{}': {e}", path.display()))?;
    parse_wav_bytes(&bytes)
}

/// Parsea bytes en formato RIFF/WAVE a muestras mono `f32` normalizadas en `[-1, 1]`.
pub fn parse_wav_bytes(bytes: &[u8]) -> Result<WavIrData, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("Encabezado RIFF/WAVE no válido.".to_string());
    }

    let mut format_tag: Option<u16> = None;
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut block_align: u16 = 0;
    let mut bits_per_sample: u16 = 0;
    let mut data_slice: Option<&[u8]> = None;

    let mut offset = 12usize;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes([
            bytes[offset + 4],
            bytes[offset + 5],
            bytes[offset + 6],
            bytes[offset + 7],
        ]) as usize;
        let body_start = offset + 8;
        let body_end = body_start
            .checked_add(size)
            .ok_or_else(|| "Tamaño de chunk WAV desbordado.".to_string())?;
        if body_end > bytes.len() {
            return Err("Archivo WAV truncado antes de terminar el chunk.".to_string());
        }
        let body = &bytes[body_start..body_end];

        if id == b"fmt " {
            if body.len() < 16 {
                return Err("Chunk 'fmt ' demasiado corto.".to_string());
            }
            let raw_tag = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]);
            sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            block_align = u16::from_le_bytes([body[12], body[13]]);
            bits_per_sample = u16::from_le_bytes([body[14], body[15]]);

            let resolved_tag = if raw_tag == 0xFFFE {
                if body.len() < 26 {
                    return Err("Chunk 'fmt ' WAVE_FORMAT_EXTENSIBLE incompleto.".to_string());
                }
                u16::from_le_bytes([body[24], body[25]])
            } else {
                raw_tag
            };
            format_tag = Some(resolved_tag);
        } else if id == b"data" {
            data_slice = Some(body);
        }

        let padded = size.saturating_add(size & 1);
        offset = body_start
            .checked_add(padded)
            .ok_or_else(|| "Offset de chunk WAV desbordado.".to_string())?;
    }

    let tag = format_tag.ok_or_else(|| "Falta el chunk 'fmt ' en el WAV.".to_string())?;
    let raw_data = data_slice.ok_or_else(|| "Falta el chunk 'data' en el WAV.".to_string())?;

    if channels == 0 || channels > 16 {
        return Err(format!("Número de canales no válido en WAV: {channels}."));
    }
    if !(4_000..=384_000).contains(&sample_rate) {
        return Err(format!(
            "Frecuencia de muestreo WAV fuera de rango: {sample_rate} Hz."
        ));
    }

    let bytes_per_sample = match (tag, bits_per_sample) {
        (1, 8) => 1usize,
        (1, 16) => 2usize,
        (1, 24) => 3usize,
        (1, 32) => 4usize,
        (3, 32) => 4usize,
        (3, 64) => 8usize,
        _ => {
            return Err(format!(
                "Formato WAV no soportado (tag={tag}, bits={bits_per_sample})."
            ));
        }
    };

    let ch_usize = channels as usize;
    let frame_bytes = (block_align as usize).max(bytes_per_sample * ch_usize);
    if frame_bytes < bytes_per_sample * ch_usize {
        return Err("Alineación de frame WAV inconsistente.".to_string());
    }

    let num_frames = raw_data.len() / frame_bytes;
    if num_frames == 0 {
        return Err("El archivo WAV no contiene muestras de audio.".to_string());
    }

    let mut mono_avg = Vec::with_capacity(num_frames);
    let mut ch0_only = Vec::with_capacity(if ch_usize > 1 { num_frames } else { 0 });
    let inv_ch = 1.0_f64 / ch_usize as f64;

    for frame in raw_data.chunks_exact(frame_bytes) {
        let mut sum = 0.0_f64;
        let mut first = 0.0_f64;
        for ch in 0..ch_usize {
            let start = ch * bytes_per_sample;
            let sample_val = decode_sample(
                &frame[start..start + bytes_per_sample],
                tag,
                bits_per_sample,
            );
            if ch == 0 {
                first = sample_val;
            }
            sum += sample_val;
        }
        mono_avg.push((sum * inv_ch) as f32);
        if ch_usize > 1 {
            ch0_only.push(first as f32);
        }
    }

    let samples = if ch_usize > 1 {
        let e_avg: f64 = mono_avg.iter().map(|&x| (x as f64) * (x as f64)).sum();
        let e_ch0: f64 = ch0_only.iter().map(|&x| (x as f64) * (x as f64)).sum();
        // Si la mezcla estéreo L+R cancela la fase de la señal, conserva el canal izquierdo.
        if e_ch0 > 1e-12 && e_avg.sqrt() < 0.25 * e_ch0.sqrt() {
            ch0_only
        } else {
            mono_avg
        }
    } else {
        mono_avg
    };

    Ok(WavIrData {
        sample_rate,
        channels,
        samples,
    })
}

#[inline]
fn decode_sample(bytes: &[u8], tag: u16, bits: u16) -> f64 {
    match (tag, bits) {
        (1, 8) => (bytes[0] as f64 - 128.0) / 128.0,
        (1, 16) => i16::from_le_bytes([bytes[0], bytes[1]]) as f64 / 32_768.0,
        (1, 24) => {
            let raw = (bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16);
            let signed = if (raw & 0x0080_0000) != 0 {
                raw | !0x00FF_FFFF
            } else {
                raw
            };
            signed as f64 / 8_388_608.0
        }
        (1, 32) => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64 / 2_147_483_648.0
        }
        (3, 32) => {
            let v = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64;
            if v.is_finite() {
                v
            } else {
                0.0
            }
        }
        (3, 64) => {
            let v = f64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            if v.is_finite() {
                v
            } else {
                0.0
            }
        }
        _ => 0.0,
    }
}

/// Codifica una señal mono en bytes RIFF/WAVE IEEE 32-bit float (útil en pruebas y utilidades).
pub fn write_wav_f32_mono(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
    let data_bytes = (samples.len() * 4) as u32;
    let riff_size = 4 + (8 + 16) + (8 + data_bytes);
    let mut out = Vec::with_capacity((riff_size + 8) as usize);

    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");

    // Chunk 'fmt ' (16 bytes, tag = 3 IEEE Float, 1 ch, 32-bit)
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&3_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate.saturating_mul(4)).to_le_bytes());
    out.extend_from_slice(&4_u16.to_le_bytes());
    out.extend_from_slice(&32_u16.to_le_bytes());

    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        let clean = if s.is_finite() { s } else { 0.0 };
        out.extend_from_slice(&clean.to_le_bytes());
    }
    out
}

/// Remuestrea una IR *offline* mediante *windowed-sinc* (Blackman de 24 cruces) truncando a `max_samples`
/// con un fundido suave al final si el archivo supera la duración máxima.
pub fn resample_ir_sinc(
    samples: &[f32],
    in_rate: u32,
    out_rate: u32,
    max_samples: usize,
) -> Vec<f32> {
    if samples.is_empty() || max_samples == 0 || in_rate == 0 || out_rate == 0 {
        return Vec::new();
    }

    let mut out = if in_rate == out_rate {
        samples
            .iter()
            .copied()
            .take(max_samples)
            .collect::<Vec<f32>>()
    } else {
        let ratio = out_rate as f64 / in_rate as f64;
        let out_len = ((samples.len() as f64) * ratio)
            .round()
            .clamp(1.0, max_samples as f64) as usize;
        let half_taps: isize = 24;
        let fc = 0.46 * (in_rate.min(out_rate) as f64 / in_rate as f64);
        let scale = 2.0 * fc;

        let mut resampled = Vec::with_capacity(out_len);
        for n in 0..out_len {
            let src_pos = (n as f64) / ratio;
            let center = src_pos.floor() as isize;
            let mut acc = 0.0_f64;
            let mut w_sum = 0.0_f64;

            for k in -half_taps..=half_taps {
                let idx = center + k;
                if idx < 0 || (idx as usize) >= samples.len() {
                    continue;
                }
                let d = src_pos - (idx as f64);
                if d.abs() >= half_taps as f64 {
                    continue;
                }
                let sinc = if d.abs() < 1e-9 {
                    scale
                } else {
                    let arg = 2.0 * PI * fc * d;
                    arg.sin() / (PI * d)
                };
                let win_u = 0.5 * (d / (half_taps as f64) + 1.0);
                let blackman =
                    0.42 - 0.5 * (2.0 * PI * win_u).cos() + 0.08 * (4.0 * PI * win_u).cos();
                let w = sinc * blackman;
                acc += (samples[idx as usize] as f64) * w;
                w_sum += w;
            }
            // Preserva ganancia unitaria en banda pasante mediante normalización directa por suma de pesos.
            let norm_val = if w_sum.abs() > 1e-9 { acc / w_sum } else { 0.0 };
            resampled.push(if norm_val.is_finite() {
                norm_val as f32
            } else {
                0.0
            });
        }
        resampled
    };

    // Fundido de salida de 5 ms si hubo truncamiento para evitar discontinuidad espectral brusca.
    let was_truncated =
        (samples.len() as f64 * (out_rate as f64 / in_rate as f64)) > out.len() as f64;
    if was_truncated && out.len() > 8 {
        let fade_len = ((0.005 * out_rate as f32).round() as usize).min(out.len() / 2);
        if fade_len > 1 {
            let start = out.len() - fade_len;
            for (i, s) in out[start..].iter_mut().enumerate() {
                let t = i as f32 / (fade_len - 1) as f32;
                let w = 0.5 * (1.0 + (std::f32::consts::PI * t).cos());
                *s *= w;
            }
        }
    }

    out
}

/// Normaliza la energía espectral de una IR usando la magnitud RMS media en `120–3500 Hz`.
///
/// Garantiza volumen homogéneo entre los 3 gabinetes integrados y archivos `.wav` externos
/// independientemente de su duración o frecuencia de muestreo.
pub fn normalize_ir_guitar_band(ir: &mut [f32], sample_rate: f32) {
    if ir.is_empty() || !sample_rate.is_finite() || sample_rate <= 0.0 {
        return;
    }

    let n_fft = ir.len().next_power_of_two().max(1024);
    let mut re = vec![0.0_f32; n_fft];
    let mut im = vec![0.0_f32; n_fft];
    for (dst, &src) in re.iter_mut().zip(ir.iter()) {
        *dst = if src.is_finite() { src } else { 0.0 };
    }
    fft_in_place(&mut re, &mut im);

    let bin_hz = sample_rate / n_fft as f32;
    let k_lo = ((120.0 / bin_hz).ceil() as usize).clamp(1, n_fft / 2 - 1);
    let k_hi = ((3_500.0_f32.min(0.45 * sample_rate) / bin_hz).floor() as usize)
        .clamp(k_lo, n_fft / 2 - 1);

    let mut mag_sq_sum = 0.0_f64;
    let mut count = 0usize;
    for k in k_lo..=k_hi {
        let m2 = (re[k] as f64) * (re[k] as f64) + (im[k] as f64) * (im[k] as f64);
        mag_sq_sum += m2;
        count += 1;
    }

    let band_rms = if count > 0 {
        (mag_sq_sum / count as f64).sqrt()
    } else {
        0.0
    };
    if band_rms > 1e-6 {
        // Limita ganancia máxima de rescate a +36 dB (~63x) por seguridad ante ruido puro.
        let gain = (1.0 / band_rms).min(63.1) as f32;
        for s in ir.iter_mut() {
            *s *= gain;
        }
    }
}
