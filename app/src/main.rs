mod analyzer_ui;
mod gui;
mod presets;
mod settings;

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
use engine::{list_devices, Engine, EngineConfig, HostKind, InitialParams, Meters, Params};
use gui::{run_gui, GuiStartOptions};

#[derive(Parser)]
#[command(
    name = "pedalera",
    version,
    about = "Pedalera virtual de guitarra en tiempo real (WASAPI + ASIO). Sin subcomando abre la interfaz gráfica."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Abre la interfaz gráfica de la pedalera (comportamiento por defecto)
    Gui(GuiArgs),
    /// Lista los hosts, dispositivos de audio y configuraciones soportadas
    List,
    /// Lista los presets de fábrica y de usuario disponibles
    Presets,
    /// Inicia el procesamiento de audio por consola en tiempo real
    Run(RunArgs),
}

#[derive(Args, Default)]
struct GuiArgs {
    /// Backend inicial (wasapi o asio)
    #[arg(long, value_enum)]
    host: Option<HostArg>,

    /// Subcadena del dispositivo de entrada inicial
    #[arg(long)]
    input: Option<String>,

    /// Subcadena del dispositivo de salida inicial
    #[arg(long)]
    output: Option<String>,

    /// Iniciar con salida silenciada
    #[arg(long, default_value_t = false)]
    mute: bool,
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
    #[arg(long, default_value_t = 12.0, allow_hyphen_values = true)]
    drive: f32,

    /// Frecuencia de tono (pasa-bajos) del overdrive en Hz (500..12000)
    #[arg(long, default_value_t = 4000.0, allow_hyphen_values = true)]
    tone: f32,

    /// Nivel de salida del overdrive en dB
    #[arg(long, default_value_t = -6.0, allow_hyphen_values = true)]
    level: f32,

    /// Umbral del noise gate en dBFS (<= -100 desactiva el gate)
    #[arg(long, default_value_t = -60.0, allow_hyphen_values = true)]
    gate: f32,

    /// Volumen maestro en dB
    #[arg(long, default_value_t = -6.0, allow_hyphen_values = true)]
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

    /// Nombre o ruta JSON del preset a cargar
    #[arg(long)]
    preset: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli
        .command
        .unwrap_or_else(|| Command::Gui(GuiArgs::default()))
    {
        Command::Gui(args) => cmd_gui(args),
        Command::List => cmd_list(),
        Command::Presets => cmd_presets(),
        Command::Run(args) => cmd_run(args),
    }
}

fn cmd_gui(args: GuiArgs) -> Result<()> {
    let host = match args.host {
        Some(h) => Some(resolve_host_kind(h)?),
        None => None,
    };
    run_gui(GuiStartOptions {
        host,
        input: args.input,
        output: args.output,
        mute: args.mute,
    })
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
    println!(
        "\nNota: esta compilación no incluye ASIO. Compila con '--features asio' para habilitarlo."
    );

    Ok(())
}

fn cmd_presets() -> Result<()> {
    println!("=== Presets de fábrica ===");
    for &(name, _) in presets::FACTORY {
        println!("  - {name}");
    }

    let dir = presets::user_presets_dir()?;
    let user = presets::list_user_presets();
    println!("\n=== Presets de usuario ({}) ===", dir.display());
    if user.is_empty() {
        println!("  (Ningún preset de usuario guardado todavía)");
    } else {
        for (name, path) in user {
            println!("  - {name} ({})", path.display());
        }
    }

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
    if let Some(ref preset_arg) = args.preset {
        let preset = presets::resolve_preset(preset_arg)?;
        let warnings = preset.apply(&params);
        for w in warnings {
            eprintln!("Aviso preset: {w}");
        }
        // `--volume` controla el Master global fuera del preset.
        params.set_volume_db(args.volume);
    }
    let meters = Arc::new(Meters::new());

    let engine = Engine::start(EngineConfig {
        host,
        input_name: args.input,
        output_name: args.output,
        input_channel: args.in_channel,
        buffer_frames: args.buffer,
        latency_frames: args.latency,
        processing_sample_rate: None,
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
    if info.is_resampling_input || info.is_resampling_output {
        println!(
            "Sample rate:    entrada {} Hz -> DSP {} Hz -> salida {} Hz",
            info.input_sample_rate, info.processing_sample_rate, info.output_sample_rate
        );
    } else {
        println!(
            "Sample rate:    {} Hz (1:1 sin resampleo)",
            info.sample_rate
        );
    }
    println!(
        "Buffers:        entrada={} frames, salida={} frames, ring prefill={} frames",
        info.input_buffer_frames
            .map_or("desconocido".to_string(), |v| v.to_string()),
        info.output_buffer_frames
            .map_or("desconocido".to_string(), |v| v.to_string()),
        info.prefill_frames
    );
    println!("Latencia est.:  {:.2} ms", info.estimated_latency_ms);
    let mut chain_parts: Vec<String> = Vec::new();
    for slot_idx in params.mounted_cascade() {
        if let Some(slot) = params.slots().get(slot_idx) {
            let p_str: Vec<String> = slot
                .specs
                .iter()
                .enumerate()
                .map(|(i, sp)| format!("{} {}", sp.name, sp.format_value(slot.get(i))))
                .collect();
            let bypass_tag = if slot.bypass() { ", BYPASS" } else { "" };
            chain_parts.push(format!(
                "{} ({}{})",
                slot.display_name(),
                p_str.join(", "),
                bypass_tag
            ));
        }
    }
    chain_parts.push(format!("Master ({:.1} dB)", params.volume_db()));
    let preset_lvl = params.preset_level_db();
    let lvl_tag = if preset_lvl.abs() > 0.05 {
        format!(" [nivel preset: {preset_lvl:+.1} dB]")
    } else {
        String::new()
    };
    println!(
        "Cadena DSP:     {}{}{}",
        chain_parts.join(" -> "),
        lvl_tag,
        if args.mute {
            " [SALIDA SILENCIADA / MUTE]"
        } else {
            ""
        }
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
