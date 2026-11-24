use std::fmt;

use anyhow::{anyhow, bail, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait},
    Device, DeviceId, ErrorKind, Host, HostId, SampleFormat, SupportedBufferSize,
    SupportedStreamConfig,
};

/// Backend de audio soportado en Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKind {
    Wasapi,
    #[cfg(feature = "asio")]
    Asio,
}

impl HostKind {
    pub fn as_cpal_id(self) -> HostId {
        match self {
            HostKind::Wasapi => HostId::Wasapi,
            #[cfg(feature = "asio")]
            HostKind::Asio => HostId::Asio,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            HostKind::Wasapi => "WASAPI",
            #[cfg(feature = "asio")]
            HostKind::Asio => "ASIO",
        }
    }
}

impl fmt::Display for HostKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

#[derive(Debug, Clone)]
pub struct ConfigSummary {
    pub channels: u16,
    pub sample_rate: u32,
    pub sample_format: SampleFormat,
    pub buffer_size: SupportedBufferSize,
}

impl ConfigSummary {
    fn from_supported(cfg: &SupportedStreamConfig) -> Self {
        Self {
            channels: cfg.channels(),
            sample_rate: cfg.sample_rate(),
            sample_format: cfg.sample_format(),
            buffer_size: *cfg.buffer_size(),
        }
    }
}

impl fmt::Display for ConfigSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let buf = match self.buffer_size {
            SupportedBufferSize::Range { min, max } => format!("{min}..={max} frames"),
            SupportedBufferSize::Unknown => "desconocido".to_string(),
        };
        write!(
            f,
            "{} ch, {} Hz, {}, buffer: {}",
            self.channels, self.sample_rate, self.sample_format, buf
        )
    }
}

#[derive(Debug, Clone)]
pub struct DeviceReport {
    pub name: String,
    pub is_default_input: bool,
    pub is_default_output: bool,
    pub input_channels: Option<u16>,
    pub output_channels: Option<u16>,
    pub default_input: Option<ConfigSummary>,
    pub default_output: Option<ConfigSummary>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HostReport {
    pub host: HostKind,
    pub devices: Vec<DeviceReport>,
    pub error: Option<String>,
}

/// Enumera los dispositivos disponibles en cada host compilado (WASAPI y opcionalmente ASIO).
/// Nunca aborta ante errores de dispositivos individuales (por ejemplo drivers ASIO virtuales).
pub fn list_devices() -> Vec<HostReport> {
    #[cfg_attr(not(feature = "asio"), allow(unused_mut))]
    let mut kinds = vec![HostKind::Wasapi];
    #[cfg(feature = "asio")]
    kinds.push(HostKind::Asio);

    kinds.into_iter().map(inspect_host).collect()
}

fn inspect_host(kind: HostKind) -> HostReport {
    let host = match cpal::host_from_id(kind.as_cpal_id()) {
        Ok(h) => h,
        Err(e) => {
            return HostReport {
                host: kind,
                devices: Vec::new(),
                error: Some(format!("No se pudo inicializar el host {kind}: {e}")),
            };
        }
    };

    let default_in_id: Option<DeviceId> = host.default_input_device().and_then(|d| d.id().ok());
    let default_out_id: Option<DeviceId> = host.default_output_device().and_then(|d| d.id().ok());

    let devices_iter = match host.devices() {
        Ok(it) => it,
        Err(e) => {
            return HostReport {
                host: kind,
                devices: Vec::new(),
                error: Some(format!("Error al enumerar dispositivos de {kind}: {e}")),
            };
        }
    };

    let mut devices = Vec::new();
    for device in devices_iter {
        devices.push(inspect_device(
            &device,
            default_in_id.as_ref(),
            default_out_id.as_ref(),
        ));
    }

    HostReport {
        host: kind,
        devices,
        error: None,
    }
}

fn device_name(device: &Device) -> String {
    match device.description() {
        Ok(desc) => desc.name().to_string(),
        Err(_) => device.to_string(),
    }
}

fn inspect_device(
    device: &Device,
    default_in_id: Option<&DeviceId>,
    default_out_id: Option<&DeviceId>,
) -> DeviceReport {
    let mut errors = Vec::new();
    let name = match device.description() {
        Ok(desc) => desc.name().to_string(),
        Err(e) => {
            errors.push(format!("No se pudo leer descripción: {e}"));
            device.to_string()
        }
    };

    let dev_id = match device.id() {
        Ok(id) => Some(id),
        Err(e) => {
            errors.push(format!("No se pudo leer ID: {e}"));
            None
        }
    };

    let is_default_input = dev_id.as_ref().is_some_and(|id| default_in_id == Some(id));
    let is_default_output = dev_id.as_ref().is_some_and(|id| default_out_id == Some(id));

    let default_input = match device.default_input_config() {
        Ok(cfg) => Some(ConfigSummary::from_supported(&cfg)),
        Err(e) if e.kind() == ErrorKind::UnsupportedOperation => None,
        Err(e) => {
            errors.push(format!("Config entrada por defecto: {e}"));
            None
        }
    };

    let mut input_channels = default_input.as_ref().map(|c| c.channels);
    match device.supported_input_configs() {
        Ok(configs) => {
            let max_ch = configs.map(|c| c.channels()).max();
            if let Some(m) = max_ch {
                input_channels = Some(input_channels.map_or(m, |c| c.max(m)));
            }
        }
        Err(e) if e.kind() == ErrorKind::UnsupportedOperation => {}
        Err(e) => errors.push(format!("Configs entrada soportadas: {e}")),
    }

    let default_output = match device.default_output_config() {
        Ok(cfg) => Some(ConfigSummary::from_supported(&cfg)),
        Err(e) if e.kind() == ErrorKind::UnsupportedOperation => None,
        Err(e) => {
            errors.push(format!("Config salida por defecto: {e}"));
            None
        }
    };

    let mut output_channels = default_output.as_ref().map(|c| c.channels);
    match device.supported_output_configs() {
        Ok(configs) => {
            let max_ch = configs.map(|c| c.channels()).max();
            if let Some(m) = max_ch {
                output_channels = Some(output_channels.map_or(m, |c| c.max(m)));
            }
        }
        Err(e) if e.kind() == ErrorKind::UnsupportedOperation => {}
        Err(e) => errors.push(format!("Configs salida soportadas: {e}")),
    }

    DeviceReport {
        name,
        is_default_input,
        is_default_output,
        input_channels,
        output_channels,
        default_input,
        default_output,
        errors,
    }
}

pub(crate) fn open_host(kind: HostKind) -> Result<Host> {
    cpal::host_from_id(kind.as_cpal_id())
        .map_err(|e| anyhow!("No se pudo abrir el host {}: {e}", kind))
}

pub(crate) fn resolve_devices(
    host: &Host,
    kind: HostKind,
    input_substr: Option<&str>,
    output_substr: Option<&str>,
) -> Result<(Device, Device, String, String)> {
    match kind {
        HostKind::Wasapi => resolve_wasapi_devices(host, input_substr, output_substr),
        #[cfg(feature = "asio")]
        HostKind::Asio => resolve_asio_device(host, input_substr, output_substr),
    }
}

fn find_matching_device<I>(devices: I, needle: &str, role: &str) -> Result<(Device, String)>
where
    I: Iterator<Item = Device>,
{
    let needle_lower = needle.to_lowercase();
    let mut available = Vec::new();
    for dev in devices {
        let name = device_name(&dev);
        if name.to_lowercase().contains(&needle_lower) {
            return Ok((dev, name));
        }
        available.push(name);
    }
    bail!(
        "No se encontró dispositivo de {role} que contenga \"{needle}\". Disponibles: [{}]",
        available.join(", ")
    );
}

fn resolve_wasapi_devices(
    host: &Host,
    input_substr: Option<&str>,
    output_substr: Option<&str>,
) -> Result<(Device, Device, String, String)> {
    let (in_dev, in_name) = match input_substr {
        Some(sub) => {
            let devs = host.input_devices()?;
            find_matching_device(devs, sub, "entrada")?
        }
        None => {
            let dev = host
                .default_input_device()
                .ok_or_else(|| anyhow!("No hay dispositivo de entrada predeterminado en WASAPI"))?;
            let name = device_name(&dev);
            (dev, name)
        }
    };

    let (out_dev, out_name) = match output_substr {
        Some(sub) => {
            let devs = host.output_devices()?;
            find_matching_device(devs, sub, "salida")?
        }
        None => {
            let dev = host
                .default_output_device()
                .ok_or_else(|| anyhow!("No hay dispositivo de salida predeterminado en WASAPI"))?;
            let name = device_name(&dev);
            (dev, name)
        }
    };

    Ok((in_dev, out_dev, in_name, out_name))
}

#[cfg(feature = "asio")]
fn resolve_asio_device(
    host: &Host,
    input_substr: Option<&str>,
    output_substr: Option<&str>,
) -> Result<(Device, Device, String, String)> {
    if let (Some(in_s), Some(out_s)) = (input_substr, output_substr) {
        if !in_s.eq_ignore_ascii_case(out_s) {
            bail!(
                "En ASIO la entrada y la salida deben usar el mismo driver/dispositivo (recibido entrada='{in_s}', salida='{out_s}')."
            );
        }
    }

    let wanted = input_substr.or(output_substr);
    let dev = match wanted {
        Some(sub) => {
            let devs = host.devices()?;
            let (d, _) = find_matching_device(devs, sub, "ASIO")?;
            d
        }
        None => {
            let devs = host.devices()?;
            let mut chosen = None;
            for d in devs {
                if d.default_input_config().is_ok() && d.default_output_config().is_ok() {
                    chosen = Some(d);
                    break;
                }
            }
            chosen.ok_or_else(|| {
                anyhow!("No se encontró ningún driver ASIO funcional con entrada y salida activas")
            })?
        }
    };

    let name = device_name(&dev);
    let out_dev = dev.clone();
    Ok((dev, out_dev, name.clone(), name))
}
