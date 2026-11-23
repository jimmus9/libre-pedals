use std::{
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use engine::{
    list_devices, Engine, EngineConfig, HostKind, InitialParams, Meters, Params,
};

#[derive(Parser)]
#[command(
    name = "pedalera",
    version,
    about = "Pedalera virtual de guitarra en tiempo real (WASAPI + ASIO)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Lista los hosts, dispositivos de audio y configuraciones soportadas
    List,
    /// Inicia el procesamiento de audio en tiempo real
    Run(RunArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum HostArg {
    Wasapi,
    Asio,
}

#[derive(Args)]
struct RunArgs {
    /// Backend de audio a utilizar (wasapi o asio)
    #[arg(long, value_enum, default_value_t = HostArg::Wasapi)]
    host: HostArg,

    /// Subcadena del nombre del dispositivo de entrada (si se omite, usa el predeterminado)
    #[arg(long)]
    input: Option<String>,

    /// Subcadena del nombre del dispositivo de salida (si se omite, usa el predeterminado)
    #[arg(long)]
    output: Option<String>,

    /// Índice del canal de entrada (base 0, por defecto 0)
    #[arg(long = "in-channel", default_value_t = 0)]
    in_channel: usize,

    /// Tamaño de buffer solicitado en frames (si falla, usa Default)
    #[arg(long)]
    buffer: Option<u32>,

    /// Prellenado del ring buffer en frames (por defecto 2x buffer o 512)
    #[arg(long)]
    latency: Option<u32>,

    /// Ganancia del overdrive en dB (0..40)
    #[arg(long, default_value_t = 12.0)]
    drive: f32,

    /// Frecuencia de tono (pasa-bajos) del overdrive en Hz (500..12000)
    #[arg(long, default_value_t = 4000.0)]
    tone: f32,

    /// Nivel de salida del overdrive en dB
    #[arg(long, default_value_t = -6.0)]
    level: f32,

    /// Umbral del noise gate en dBFS (<= -100 desactiva el gate)
    #[arg(long, default_value_t = -60.0)]
    gate: f32,

    /// Volumen maestro en dB
    #[arg(long, default_value_t = -6.0)]
    volume: f32,

    /// Duración máxima de ejecución en segundos (si se omite, corre hasta Ctrl+C)
    #[arg(long)]
    seconds: Option<u64>,

    /// Procesa el audio pero silencia la salida física (útil para pruebas sin acoples)
    #[arg(long, default_value_t = false)]
    mute: bool,

    /// Desactiva (bypass) el pedal de overdrive
    #[arg(long = "bypass-drive", default_value_t = false)]
    bypass_drive: bool,

    /// Desactiva (bypass) el noise gate
    #[arg(long = "bypass-gate", default_value_t = false)]
    bypass_gate: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List => cmd_list(),
        Command::Run(args) => cmd_run(args),
    }
}

fn cmd_list() -> Result<()> {
    let reports = list_devices();
    for host in reports {
        println!("=== {} ===", host.host);
        if let Some(err) = &host.error {
            println!("  Error del host: {err}");
            continue;
        }
        if host.devices.is_empty() {
            println!("  (No se encontraron dispositivos activos en este host)");
            continue;
        }
        for (idx, dev) in host.devices.iter().enumerate() {
            let mut tags = Vec::new();
            if dev.is_default_input {
                tags.push("default IN");
            }
            if dev.is_default_output {
                tags.push("default OUT");
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            println!(
                "  {}. {}{tag_str} (in_ch: {}, out_ch: {})",
                idx + 1,
                dev.name,
                dev.input_channels
                    .map_or("-".to_string(), |c| c.to_string()),
                dev.output_channels
                    .map_or("-".to_string(), |c| c.to_string()),
            );
            if let Some(cfg) = &dev.default_input {
                println!("     Entrada default: {cfg}");
            }
            if let Some(cfg) = &dev.default_output {
                println!("     Salida default:  {cfg}");
            }
            for err in &dev.errors {
                println!("     Aviso/Error:     {err}");
            }
        }
    }

    #[cfg(not(feature = "asio"))]
    println!("\nNota: esta compilación no incluye ASIO. Compila con '--features asio' para habilitarlo.");

    Ok(())
}

fn resolve_host_kind(arg: HostArg) -> Result<HostKind> {
    match arg {
        HostArg::Wasapi => Ok(HostKind::Wasapi),
        HostArg::Asio => {
            #[cfg(feature = "asio")]
            {
                Ok(HostKind::Asio)
            }
            #[cfg(not(feature = "asio"))]
            {
                anyhow::bail!(
                    "El host ASIO no está habilitado en este binario. Recompila con: cargo build --release --features asio"
                );
            }
        }
    }
}

fn cmd_run(args: RunArgs) -> Result<()> {
    let host = resolve_host_kind(args.host)?;

    let init = InitialParams {
        drive_db: args.drive,
        tone_hz: args.tone,
        level_db: args.level,
        gate_threshold_db: args.gate,
        volume_db: args.volume,
        mute: args.mute,
        bypass_gate: args.bypass_gate,
        bypass_drive: args.bypass_drive,
        bypass_volume: false,
    };

    let params = Arc::new(Params::from_initial(init));
    let meters = Arc::new(Meters::new());

    let engine = Engine::start(EngineConfig {
        host,
        input_name: args.input,
        output_name: args.output,
        input_channel: args.in_channel,
        buffer_frames: args.buffer,
        latency_frames: args.latency,
        params: Arc::clone(&params),
        meters: Arc::clone(&meters),
    })?;

    let info = engine.info();
    println!("=== Pedalera iniciada ===");
    println!("Host:           {}", info.host);
    println!(
        "Entrada:        {} (canal {}/{}, formato {})",
        info.input_device, info.input_channel_used, info.input_channels, info.input_format
    );
    println!(
        "Salida:         {} ({} ch, formato {})",
        info.output_device, info.output_channels, info.output_format
    );
    println!("Sample rate:    {} Hz", info.sample_rate);
    println!(
        "Buffers:        entrada={} frames, salida={} frames, ring prefill={} frames",
        info.input_buffer_frames
            .map_or("desconocido".to_string(), |v| v.to_string()),
        info.output_buffer_frames
            .map_or("desconocido".to_string(), |v| v.to_string()),
        info.prefill_frames
    );
    println!("Latencia est.:  {:.2} ms", info.estimated_latency_ms);
    println!(
        "Cadena DSP:     NoiseGate ({:.1} dB{}) -> Overdrive (drive {:.1} dB, tone {:.0} Hz, level {:.1} dB{}) -> Volume ({:.1} dB){}",
        args.gate,
        if args.bypass_gate { ", BYPASS" } else { "" },
        args.drive,
        args.tone,
        args.level,
        if args.bypass_drive { ", BYPASS" } else { "" },
        args.volume,
        if args.mute { " [SALIDA SILENCIADA / MUTE]" } else { "" }
    );
    println!("Presiona Ctrl+C para detener.\n");

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = Arc::clone(&running);
        let _ = ctrlc::set_handler(move || {
            running.store(false, Ordering::SeqCst);
        });
    }

    let start = Instant::now();
    let max_duration = args.seconds.map(Duration::from_secs);
    let mut stdout = io::stdout();

    while running.load(Ordering::SeqCst) {
        if let Some(limit) = max_duration {
            if start.elapsed() >= limit {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));

        let snap = meters.snapshot_and_reset_peaks();
        let xruns = snap.underruns + snap.overflows;
        let mute_tag = if args.mute { " [MUTE]" } else { "" };
        print!(
            "\rIn: {:>6.1} dBFS | Out: {:>6.1} dBFS | xruns: {} (und: {}, ovf: {}, drift: {}, err: {}){}   ",
            snap.input_db,
            snap.output_db,
            xruns,
            snap.underruns,
            snap.overflows,
            snap.drift_drops,
            snap.stream_errors,
            mute_tag
        );
        let _ = stdout.flush();
    }

    println!("\nDetenido.");
    Ok(())
}
