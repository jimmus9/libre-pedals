pub mod devices;
pub mod params;
pub mod preset;

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

use anyhow::{anyhow, bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, StreamTrait},
    BufferSize, Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedBufferSize, SupportedStreamConfig,
};
use dsp::{
    db_to_lin, BrickwallLimiter, Chain, CleanBoost, Effect, Gain, NoiseGate, OptoCompressor,
    Overdrive, Resampler, ScopedNoDenormals, SoftClipTape, TubePreamp,
};
use rtrb::{Consumer, Producer, RingBuffer};

pub use devices::{list_devices, ConfigSummary, DeviceReport, HostKind, HostReport};
pub use params::{
    InitialParams, KindInfo, MeterSnapshot, Meters, Params, PedalParamSlot, TestSignalMode,
    MAX_INSTANCES_PER_KIND, MAX_MOUNTED_PEDALS, PRESET_LEVEL_MAX_DB, PRESET_LEVEL_MIN_DB,
};
pub use preset::{Preset, PresetPedal, PRESET_FORMAT_VERSION};

const SCRATCH_FRAMES: usize = 8192;
const MICRO_BLOCK_FRAMES: usize = 64;
const ANALYSIS_RING_CAPACITY: usize = 65_536;

/// Parámetros de arranque del motor de audio.
pub struct EngineConfig {
    pub host: HostKind,
    pub input_name: Option<String>,
    pub output_name: Option<String>,
    pub input_channel: usize,
    pub buffer_frames: Option<u32>,
    pub latency_frames: Option<u32>,
    /// Resolución interna de procesado DSP en Hz. `None` usa la nativa de entrada sin resamplear.
    pub processing_sample_rate: Option<u32>,
    pub params: Arc<Params>,
    pub meters: Arc<Meters>,
}

/// Información de los streams abiertos y estimación de latencia.
#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub host: HostKind,
    pub input_device: String,
    pub output_device: String,
    pub sample_rate: u32,
    pub input_sample_rate: u32,
    pub processing_sample_rate: u32,
    pub output_sample_rate: u32,
    pub is_resampling_input: bool,
    pub is_resampling_output: bool,
    pub input_channels: u16,
    pub output_channels: u16,
    pub input_channel_used: usize,
    pub input_format: SampleFormat,
    pub output_format: SampleFormat,
    pub input_buffer_frames: Option<u32>,
    pub output_buffer_frames: Option<u32>,
    pub prefill_frames: u32,
    pub estimated_latency_ms: f32,
    pub warnings: Vec<String>,
}

/// Motor de audio activo. Mantener vivo mientras dure el procesamiento.
pub struct Engine {
    info: EngineInfo,
    analysis_consumer: Consumer<(f32, f32)>,
    _input_stream: Stream,
    _output_stream: Stream,
}

impl Engine {
    pub fn info(&self) -> &EngineInfo {
        &self.info
    }

    /// Drena todos los pares `(pre_limpio, post_efectos)` disponibles para osciloscopio y FFT.
    pub fn poll_analysis(&mut self, mut on_pair: impl FnMut(f32, f32)) {
        while let Ok((pre, post)) = self.analysis_consumer.pop() {
            on_pair(pre, post);
        }
    }

    /// Abre los dispositivos elegidos, configura la cadena DSP y arranca los streams.
    pub fn start(cfg: EngineConfig) -> Result<Self> {
        let host = devices::open_host(cfg.host)?;
        let (in_dev, out_dev, in_name, out_name) = devices::resolve_devices(
            &host,
            cfg.host,
            cfg.input_name.as_deref(),
            cfg.output_name.as_deref(),
        )?;

        let in_default = in_dev
            .default_input_config()
            .with_context(|| format!("El dispositivo de entrada '{in_name}' no soporta captura"))?;
        let out_default = out_dev.default_output_config().with_context(|| {
            format!("El dispositivo de salida '{out_name}' no soporta reproducción")
        })?;

        let (in_supported, out_supported) = resolve_native_configs(
            &in_dev,
            &out_dev,
            &in_name,
            &out_name,
            &in_default,
            &out_default,
        )?;

        let in_sample_rate = in_supported.sample_rate();
        let out_sample_rate = out_supported.sample_rate();
        let proc_sample_rate = match cfg.processing_sample_rate {
            None => in_sample_rate,
            Some(req_sr) => {
                if req_sr == 0 {
                    bail!("La resolución de procesado debe ser mayor a 0 Hz.");
                }
                if req_sr > in_sample_rate {
                    bail!(
                        "La resolución de procesado solicitada ({req_sr} Hz) supera la frecuencia nativa \
                         de entrada '{in_name}' ({in_sample_rate} Hz). No se puede procesar por encima de la resolución original."
                    );
                }
                req_sr
            }
        };

        let in_channels = in_supported.channels();
        let out_channels = out_supported.channels();

        if cfg.input_channel >= in_channels as usize {
            bail!(
                "El canal de entrada {} está fuera de rango ('{}' tiene {} canales: 0..{}).",
                cfg.input_channel,
                in_name,
                in_channels,
                in_channels.saturating_sub(1)
            );
        }

        let in_format = in_supported.sample_format();
        let out_format = out_supported.sample_format();

        let in_nominal = cfg
            .buffer_frames
            .or_else(|| exact_buffer_size(in_supported.buffer_size()));
        let out_nominal = cfg
            .buffer_frames
            .or_else(|| exact_buffer_size(out_supported.buffer_size()));

        let in_proc_buf = in_nominal.map(|b| scale_frames(b, in_sample_rate, proc_sample_rate));
        let out_proc_buf = out_nominal.map(|b| scale_frames(b, out_sample_rate, proc_sample_rate));
        let nominal_proc_buf = match (in_proc_buf, out_proc_buf) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };

        let prefill_frames = cfg
            .latency_frames
            .unwrap_or_else(|| match nominal_proc_buf {
                Some(b) if b > 0 => b.saturating_mul(2),
                _ => 512_u32.saturating_mul((proc_sample_rate / 48_000).max(1)),
            });

        let ring_capacity = (prefill_frames as usize).saturating_mul(8).max(32_768);
        let drift_control = matches!(cfg.host, HostKind::Wasapi);
        let initial_drift_buf = nominal_proc_buf.unwrap_or(256);
        let max_ring_fill = Arc::new(AtomicU32::new(
            prefill_frames.saturating_add(initial_drift_buf.saturating_mul(2)),
        ));

        let mut warnings = Vec::new();
        let requested_buf = cfg
            .buffer_frames
            .map(BufferSize::Fixed)
            .unwrap_or(BufferSize::Default);

        let (input_stream, output_stream, analysis_consumer) = build_stream_pair_with_fallback(
            &in_dev,
            &out_dev,
            &in_supported,
            &out_supported,
            proc_sample_rate,
            requested_buf,
            cfg.input_channel,
            ring_capacity,
            prefill_frames,
            drift_control,
            &cfg.params,
            &cfg.meters,
            &max_ring_fill,
            &mut warnings,
        )?;

        let input_buffer_frames = input_stream.buffer_size().ok();
        let output_buffer_frames = output_stream.buffer_size().ok();

        let actual_in_proc =
            input_buffer_frames.map(|b| scale_frames(b, in_sample_rate, proc_sample_rate));
        let actual_out_proc =
            output_buffer_frames.map(|b| scale_frames(b, out_sample_rate, proc_sample_rate));
        let drift_buf = match (actual_in_proc, actual_out_proc) {
            (Some(a), Some(b)) => a.max(b),
            (a, b) => a.or(b).or(nominal_proc_buf).unwrap_or(256),
        };
        max_ring_fill.store(
            prefill_frames.saturating_add(drift_buf.saturating_mul(2)),
            Ordering::Release,
        );

        input_stream
            .play()
            .context("Error al iniciar stream de entrada")?;
        output_stream
            .play()
            .context("Error al iniciar stream de salida")?;

        let in_lat_frames = input_buffer_frames.or(cfg.buffer_frames).unwrap_or(0);
        let out_lat_frames = output_buffer_frames.or(cfg.buffer_frames).unwrap_or(0);
        let in_lat_ms = in_lat_frames as f32 * 1_000.0 / in_sample_rate as f32;
        let prefill_ms = prefill_frames as f32 * 1_000.0 / proc_sample_rate as f32;
        let out_lat_ms = out_lat_frames as f32 * 1_000.0 / out_sample_rate as f32;
        let estimated_latency_ms = in_lat_ms + prefill_ms + out_lat_ms;

        let info = EngineInfo {
            host: cfg.host,
            input_device: in_name,
            output_device: out_name,
            sample_rate: proc_sample_rate,
            input_sample_rate: in_sample_rate,
            processing_sample_rate: proc_sample_rate,
            output_sample_rate: out_sample_rate,
            is_resampling_input: in_sample_rate != proc_sample_rate,
            is_resampling_output: proc_sample_rate != out_sample_rate,
            input_channels: in_channels,
            output_channels: out_channels,
            input_channel_used: cfg.input_channel,
            input_format: in_format,
            output_format: out_format,
            input_buffer_frames,
            output_buffer_frames,
            prefill_frames,
            estimated_latency_ms,
            warnings,
        };

        Ok(Self {
            info,
            analysis_consumer,
            _input_stream: input_stream,
            _output_stream: output_stream,
        })
    }
}

#[inline]
fn scale_frames(frames: u32, from_sr: u32, to_sr: u32) -> u32 {
    if from_sr == to_sr || from_sr == 0 || to_sr == 0 {
        frames
    } else {
        ((frames as f64) * (to_sr as f64 / from_sr as f64))
            .ceil()
            .max(1.0) as u32
    }
}

type StreamPairResult = Result<(Stream, Stream, Consumer<(f32, f32)>)>;

#[allow(clippy::too_many_arguments)]
fn build_stream_pair_with_fallback(
    in_dev: &Device,
    out_dev: &Device,
    in_supported: &SupportedStreamConfig,
    out_supported: &SupportedStreamConfig,
    proc_sample_rate: u32,
    requested_buf: BufferSize,
    input_channel: usize,
    ring_capacity: usize,
    prefill_frames: u32,
    drift_control: bool,
    params: &Arc<Params>,
    meters: &Arc<Meters>,
    max_ring_fill: &Arc<AtomicU32>,
    warnings: &mut Vec<String>,
) -> StreamPairResult {
    let mut in_buf_mode = requested_buf;
    let mut out_buf_mode = requested_buf;

    for attempt in 0..3 {
        let (producer, consumer) = create_prefilled_ring(ring_capacity, prefill_frames);
        let (analysis_producer, analysis_consumer) =
            RingBuffer::<(f32, f32)>::new(ANALYSIS_RING_CAPACITY);

        let mut in_cfg = in_supported.config();
        in_cfg.buffer_size = in_buf_mode;
        let in_res = build_input_for_format(
            in_dev,
            in_cfg,
            in_supported.sample_format(),
            proc_sample_rate,
            input_channel,
            producer,
            Arc::clone(meters),
        );

        let input_stream = match in_res {
            Ok(s) => s,
            Err(e) if attempt < 2 && in_buf_mode != BufferSize::Default => {
                let warn = format!(
                    "No se pudo abrir entrada con buffer {:?} ({e}); reintentando con Default.",
                    in_buf_mode
                );
                eprintln!("Aviso: {warn}");
                warnings.push(warn);
                in_buf_mode = BufferSize::Default;
                continue;
            }
            Err(e) => return Err(e).context("Error al abrir stream de entrada"),
        };

        // Preparamos la cadena DSP a la frecuencia de procesamiento (`proc_sample_rate`).
        let mut chain = build_pedalboard_chain(params);
        let max_block = if proc_sample_rate == out_supported.sample_rate() {
            SCRATCH_FRAMES
        } else {
            MICRO_BLOCK_FRAMES
        };
        chain.prepare(proc_sample_rate as f32, max_block);

        let mut out_cfg = out_supported.config();
        out_cfg.buffer_size = out_buf_mode;
        let out_res = build_output_for_format(
            out_dev,
            out_cfg,
            out_supported.sample_format(),
            proc_sample_rate,
            consumer,
            analysis_producer,
            chain,
            Arc::clone(params),
            Arc::clone(meters),
            drift_control,
            prefill_frames as usize,
            Arc::clone(max_ring_fill),
        );

        match out_res {
            Ok(output_stream) => return Ok((input_stream, output_stream, analysis_consumer)),
            Err(e) if attempt < 2 && out_buf_mode != BufferSize::Default => {
                let warn = format!(
                    "No se pudo abrir salida con buffer {:?} ({e}); reintentando con Default.",
                    out_buf_mode
                );
                eprintln!("Aviso: {warn}");
                warnings.push(warn);
                drop(input_stream);
                out_buf_mode = BufferSize::Default;
            }
            Err(e) => return Err(e).context("Error al abrir stream de salida"),
        }
    }

    bail!("No se pudieron construir los streams de audio")
}

fn create_prefilled_ring(capacity: usize, prefill_frames: u32) -> (Producer<f32>, Consumer<f32>) {
    let (mut producer, consumer) = RingBuffer::<f32>::new(capacity);
    for _ in 0..prefill_frames {
        let _ = producer.push(0.0);
    }
    (producer, consumer)
}

fn exact_buffer_size(buf: &SupportedBufferSize) -> Option<u32> {
    match buf {
        SupportedBufferSize::Range { min, max } if min == max => Some(*min),
        _ => None,
    }
}

fn resolve_native_configs(
    in_dev: &Device,
    out_dev: &Device,
    in_name: &str,
    out_name: &str,
    in_default: &SupportedStreamConfig,
    out_default: &SupportedStreamConfig,
) -> Result<(SupportedStreamConfig, SupportedStreamConfig)> {
    // Si ambos reportan frecuencia nativa válida, respetamos la frecuencia nativa de entrada
    // (sin degradarla ni resamplear antes de los pedales; el resampleo si hiciera falta va al final).
    if in_default.sample_rate() > 0 && out_default.sample_rate() > 0 {
        return Ok((*in_default, *out_default));
    }

    if out_default.sample_rate() > 0 {
        let target_sr = out_default.sample_rate();
        if let Some(in_cfg) =
            pick_config_for_rate(in_default, in_dev.supported_input_configs(), target_sr)
        {
            return Ok((in_cfg, *out_default));
        }
    }

    // Algunos drivers ASIO devuelven 0 Hz antes de abrir el stream; probamos frecuencias estándar.
    for candidate_sr in [48_000_u32, 44_100, 96_000] {
        if let (Some(in_cfg), Some(out_cfg)) = (
            pick_config_for_rate(in_default, in_dev.supported_input_configs(), candidate_sr),
            pick_config_for_rate(
                out_default,
                out_dev.supported_output_configs(),
                candidate_sr,
            ),
        ) {
            return Ok((in_cfg, out_cfg));
        }
    }

    Err(anyhow!(
        "No se encontró una frecuencia de muestreo válida entre '{}' y '{}'.",
        in_name,
        out_name
    ))
}

fn pick_config_for_rate<I>(
    default_cfg: &SupportedStreamConfig,
    supported_iter: Result<I, cpal::Error>,
    target_sr: u32,
) -> Option<SupportedStreamConfig>
where
    I: Iterator<Item = cpal::SupportedStreamConfigRange>,
{
    if default_cfg.sample_rate() == target_sr {
        return Some(*default_cfg);
    }
    let ranges = supported_iter.ok()?;
    let mut best: Option<SupportedStreamConfig> = None;
    for range in ranges {
        if range.contains_rate(target_sr) {
            let candidate = range.with_sample_rate(target_sr);
            let replace = match &best {
                None => true,
                Some(prev) => {
                    let same_ch_new = candidate.channels() == default_cfg.channels();
                    let same_ch_old = prev.channels() == default_cfg.channels();
                    (same_ch_new && !same_ch_old)
                        || (same_ch_new == same_ch_old
                            && candidate.sample_format() == SampleFormat::F32
                            && prev.sample_format() != SampleFormat::F32)
                }
            };
            if replace {
                best = Some(candidate);
            }
        }
    }
    best
}

/// Crea la cadena de pedales por defecto prealojando hasta `MAX_INSTANCES_PER_KIND` copias
/// por tipo de pedal y un `Master Volume` fijo al final.
pub fn default_pedalboard_chain() -> Chain {
    let mut chain = Chain::new();
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(NoiseGate::new(-60.0), false);
    }
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(CleanBoost::new(), false);
    }
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(OptoCompressor::new(), false);
    }
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(Overdrive::new(), false);
    }
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(TubePreamp::new(), false);
    }
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(SoftClipTape::new(), false);
    }
    for _ in 0..MAX_INSTANCES_PER_KIND {
        chain.push(BrickwallLimiter::new(), false);
    }
    chain.push(Gain::new(-6.0), false);
    chain
}

#[inline]
fn apply_level_ramp(block: &mut [f32], current_gain: &mut f32, target_gain: f32) {
    if block.is_empty() {
        return;
    }
    if (*current_gain - target_gain).abs() < 1e-6 {
        *current_gain = target_gain;
        if (target_gain - 1.0).abs() > 1e-6 {
            for s in block.iter_mut() {
                *s *= target_gain;
            }
        }
        return;
    }
    let len = block.len() as f32;
    let start = *current_gain;
    let delta = target_gain - start;
    for (i, s) in block.iter_mut().enumerate() {
        let t = (i + 1) as f32 / len;
        *s *= start + delta * t;
    }
    *current_gain = target_gain;
}

fn build_pedalboard_chain(params: &Params) -> Chain {
    let mut chain = default_pedalboard_chain();
    sync_chain_from_params(&mut chain, params);
    chain
}

#[inline]
fn sync_chain_from_params(chain: &mut Chain, params: &Params) {
    chain.sync_order_from(|pos| params.order_at(pos));
    for (slot_idx, slot_params) in params.slots().iter().enumerate() {
        if let Some((fx, bypass)) = chain.slot_mut(slot_idx) {
            *bypass = slot_params.bypass() || !slot_params.is_mounted();
            for p_idx in 0..slot_params.param_count() {
                fx.set_param(p_idx, slot_params.get(p_idx));
            }
        }
    }
}

fn build_input_for_format(
    dev: &Device,
    config: StreamConfig,
    format: SampleFormat,
    proc_sample_rate: u32,
    input_channel: usize,
    producer: Producer<f32>,
    meters: Arc<Meters>,
) -> Result<Stream> {
    match format {
        SampleFormat::F32 => build_input_typed::<f32>(
            dev,
            config,
            proc_sample_rate,
            input_channel,
            producer,
            meters,
        ),
        SampleFormat::I16 => build_input_typed::<i16>(
            dev,
            config,
            proc_sample_rate,
            input_channel,
            producer,
            meters,
        ),
        SampleFormat::I32 => build_input_typed::<i32>(
            dev,
            config,
            proc_sample_rate,
            input_channel,
            producer,
            meters,
        ),
        SampleFormat::U16 => build_input_typed::<u16>(
            dev,
            config,
            proc_sample_rate,
            input_channel,
            producer,
            meters,
        ),
        SampleFormat::F64 => build_input_typed::<f64>(
            dev,
            config,
            proc_sample_rate,
            input_channel,
            producer,
            meters,
        ),
        other => bail!("Formato de muestra de entrada no soportado: {other}"),
    }
}

fn build_input_typed<T>(
    dev: &Device,
    config: StreamConfig,
    proc_sample_rate: u32,
    input_channel: usize,
    mut producer: Producer<f32>,
    meters: Arc<Meters>,
) -> Result<Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    let in_ch = input_channel.min(channels.saturating_sub(1));
    let mut in_resampler = Resampler::new(config.sample_rate, proc_sample_rate);
    let meters_err = Arc::clone(&meters);

    let stream = dev.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut peak = 0.0_f32;
            let mut overflowed = false;
            if in_resampler.is_passthrough() {
                for frame in data.chunks_exact(channels) {
                    let s = frame[in_ch].to_sample::<f32>();
                    let abs = s.abs();
                    if abs > peak {
                        peak = abs;
                    }
                    if producer.push(s).is_err() {
                        overflowed = true;
                    }
                }
            } else {
                for frame in data.chunks_exact(channels) {
                    let s = frame[in_ch].to_sample::<f32>();
                    let abs = s.abs();
                    if abs > peak {
                        peak = abs;
                    }
                    in_resampler.process_sample(s, |resampled| {
                        if producer.push(resampled).is_err() {
                            overflowed = true;
                        }
                    });
                }
            }
            if overflowed {
                meters.overflows.fetch_add(1, Ordering::Relaxed);
            }
            meters.input_peak.fetch_max_abs(peak);
        },
        move |_err| {
            meters_err.stream_errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )?;

    Ok(stream)
}

#[allow(clippy::too_many_arguments)]
fn build_output_for_format(
    dev: &Device,
    config: StreamConfig,
    format: SampleFormat,
    proc_sample_rate: u32,
    consumer: Consumer<f32>,
    analysis_producer: Producer<(f32, f32)>,
    chain: Chain,
    params: Arc<Params>,
    meters: Arc<Meters>,
    drift_control: bool,
    target_fill: usize,
    max_ring_fill: Arc<AtomicU32>,
) -> Result<Stream> {
    match format {
        SampleFormat::F32 => build_output_typed::<f32>(
            dev,
            config,
            proc_sample_rate,
            consumer,
            analysis_producer,
            chain,
            params,
            meters,
            drift_control,
            target_fill,
            max_ring_fill,
        ),
        SampleFormat::I16 => build_output_typed::<i16>(
            dev,
            config,
            proc_sample_rate,
            consumer,
            analysis_producer,
            chain,
            params,
            meters,
            drift_control,
            target_fill,
            max_ring_fill,
        ),
        SampleFormat::I32 => build_output_typed::<i32>(
            dev,
            config,
            proc_sample_rate,
            consumer,
            analysis_producer,
            chain,
            params,
            meters,
            drift_control,
            target_fill,
            max_ring_fill,
        ),
        SampleFormat::U16 => build_output_typed::<u16>(
            dev,
            config,
            proc_sample_rate,
            consumer,
            analysis_producer,
            chain,
            params,
            meters,
            drift_control,
            target_fill,
            max_ring_fill,
        ),
        SampleFormat::F64 => build_output_typed::<f64>(
            dev,
            config,
            proc_sample_rate,
            consumer,
            analysis_producer,
            chain,
            params,
            meters,
            drift_control,
            target_fill,
            max_ring_fill,
        ),
        other => bail!("Formato de muestra de salida no soportado: {other}"),
    }
}

#[inline]
fn next_test_sample(
    mode: TestSignalMode,
    freq_hz: f32,
    level_lin: f32,
    proc_sr: f64,
    phase1: &mut f64,
    phase2: &mut f64,
) -> f32 {
    let two_pi = std::f64::consts::TAU;
    match mode {
        TestSignalMode::Off => 0.0,
        TestSignalMode::Sine | TestSignalMode::AliasingSine => {
            let f = if matches!(mode, TestSignalMode::AliasingSine) {
                4_370.0
            } else {
                freq_hz as f64
            };
            let s = ((*phase1).sin() as f32) * level_lin;
            *phase1 = (*phase1 + two_pi * f / proc_sr) % two_pi;
            s
        }
        TestSignalMode::TwoToneImd => {
            let s = 0.5 * (((*phase1).sin() + (*phase2).sin()) as f32) * level_lin;
            *phase1 = (*phase1 + two_pi * 220.0 / proc_sr) % two_pi;
            *phase2 = (*phase2 + two_pi * 330.0 / proc_sr) % two_pi;
            s
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_output_typed<T>(
    dev: &Device,
    config: StreamConfig,
    proc_sample_rate: u32,
    mut consumer: Consumer<f32>,
    mut analysis_producer: Producer<(f32, f32)>,
    mut chain: Chain,
    params: Arc<Params>,
    meters: Arc<Meters>,
    drift_control: bool,
    target_fill: usize,
    max_ring_fill: Arc<AtomicU32>,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let out_channels = config.channels as usize;
    let out_sample_rate = config.sample_rate;
    let mut scratch = vec![0.0_f32; SCRATCH_FRAMES];
    let mut pre_scratch = vec![0.0_f32; SCRATCH_FRAMES];
    let mut out_resampler = Resampler::new(proc_sample_rate, out_sample_rate);
    let mut fifo_buf = vec![0.0_f32; 16_384];
    let mut fifo_len = 0usize;
    let mut level_gain = db_to_lin(params.preset_level_db());
    let mut test_phase1 = 0.0_f64;
    let mut test_phase2 = 0.0_f64;
    let proc_sr_f64 = (proc_sample_rate as f64).max(1.0);
    let meters_err = Arc::clone(&meters);

    let stream = dev.build_output_stream(
        config,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let _no_denormals = ScopedNoDenormals::new();
            let frames_needed = data.len() / out_channels;
            if frames_needed == 0 {
                return;
            }

            sync_chain_from_params(&mut chain, &params);
            let muted = params.mute();
            let test_mode = params.test_signal_mode();
            let test_freq = params.test_freq_hz();
            let test_level = db_to_lin(params.test_level_db());

            if drift_control {
                let min_keep_proc =
                    scale_frames(frames_needed as u32, out_sample_rate, proc_sample_rate) as usize;
                let max_fill = max_ring_fill.load(Ordering::Relaxed) as usize;
                let avail = consumer.slots();
                if avail > max_fill && avail > min_keep_proc {
                    let excess = avail
                        .saturating_sub(target_fill)
                        .min(avail.saturating_sub(min_keep_proc));
                    if excess > 0 {
                        for _ in 0..excess {
                            let _ = consumer.pop();
                        }
                        meters.drift_drops.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            let mut out_peak = 0.0_f32;
            let mut had_underrun = false;

            if out_resampler.is_passthrough() {
                // Camino directo 1:1 sin ningún resampleo.
                let mut frame_offset = 0;
                while frame_offset < frames_needed {
                    let chunk_frames = (frames_needed - frame_offset).min(scratch.len());
                    let block = &mut scratch[..chunk_frames];
                    let pre_block = &mut pre_scratch[..chunk_frames];

                    for (slot, pre_slot) in block.iter_mut().zip(pre_block.iter_mut()) {
                        let input_s = match consumer.pop() {
                            Ok(v) => v,
                            Err(_) => {
                                had_underrun = true;
                                0.0
                            }
                        };
                        let s = if test_mode == TestSignalMode::Off {
                            input_s
                        } else {
                            next_test_sample(
                                test_mode,
                                test_freq,
                                test_level,
                                proc_sr_f64,
                                &mut test_phase1,
                                &mut test_phase2,
                            )
                        };
                        *slot = s;
                        *pre_slot = s;
                    }

                    chain.process(block);
                    apply_level_ramp(block, &mut level_gain, db_to_lin(params.preset_level_db()));

                    for (i, (&pre, &post)) in pre_block.iter().zip(block.iter()).enumerate() {
                        let _ = analysis_producer.push((pre, post));
                        let out_f32 = if muted { 0.0 } else { post };
                        let abs = out_f32.abs();
                        if abs > out_peak {
                            out_peak = abs;
                        }
                        let converted = out_f32.clamp(-1.0, 1.0).to_sample::<T>();
                        let base = (frame_offset + i) * out_channels;
                        for ch in 0..out_channels {
                            data[base + ch] = converted;
                        }
                    }

                    frame_offset += chunk_frames;
                }
            } else {
                // Todo el DSP y el análisis corren a `proc_sample_rate`; el resampleo ocurre
                // estrictamente al final de la cadena antes de escribir al dispositivo de salida.
                let needed_capped = frames_needed.min(fifo_buf.len());
                while fifo_len < needed_capped {
                    let block = &mut scratch[..MICRO_BLOCK_FRAMES];
                    let pre_block = &mut pre_scratch[..MICRO_BLOCK_FRAMES];

                    for (slot, pre_slot) in block.iter_mut().zip(pre_block.iter_mut()) {
                        let input_s = match consumer.pop() {
                            Ok(v) => v,
                            Err(_) => {
                                had_underrun = true;
                                0.0
                            }
                        };
                        let s = if test_mode == TestSignalMode::Off {
                            input_s
                        } else {
                            next_test_sample(
                                test_mode,
                                test_freq,
                                test_level,
                                proc_sr_f64,
                                &mut test_phase1,
                                &mut test_phase2,
                            )
                        };
                        *slot = s;
                        *pre_slot = s;
                    }

                    chain.process(block);
                    apply_level_ramp(block, &mut level_gain, db_to_lin(params.preset_level_db()));

                    for (&pre, &post) in pre_block.iter().zip(block.iter()) {
                        let _ = analysis_producer.push((pre, post));
                        out_resampler.process_sample(post, |y_out| {
                            if fifo_len < fifo_buf.len() {
                                fifo_buf[fifo_len] = y_out;
                                fifo_len += 1;
                            }
                        });
                    }
                }

                let write_count = frames_needed.min(fifo_len);
                for (i, &y_out) in fifo_buf[..write_count].iter().enumerate() {
                    let out_f32 = if muted { 0.0 } else { y_out };
                    let abs = out_f32.abs();
                    if abs > out_peak {
                        out_peak = abs;
                    }
                    let converted = out_f32.clamp(-1.0, 1.0).to_sample::<T>();
                    let base = i * out_channels;
                    for ch in 0..out_channels {
                        data[base + ch] = converted;
                    }
                }
                for i in write_count..frames_needed {
                    let converted = 0.0_f32.to_sample::<T>();
                    let base = i * out_channels;
                    for ch in 0..out_channels {
                        data[base + ch] = converted;
                    }
                }

                let rem = fifo_len.saturating_sub(write_count);
                if rem > 0 {
                    fifo_buf.copy_within(write_count..fifo_len, 0);
                }
                fifo_len = rem;
            }

            if had_underrun {
                meters.underruns.fetch_add(1, Ordering::Relaxed);
            }
            meters.output_peak.fetch_max_abs(out_peak);
        },
        move |_err| {
            meters_err.stream_errors.fetch_add(1, Ordering::Relaxed);
        },
        None,
    )?;

    Ok(stream)
}
