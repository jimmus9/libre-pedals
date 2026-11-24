use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use engine::HostKind;
use serde::{Deserialize, Serialize};

use crate::presets::app_data_dir;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
#[serde(default)]
pub struct PresetRef {
    pub source: String, // "factory" | "user"
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Settings {
    pub host: Option<String>, // "wasapi" | "asio"
    pub input: Option<String>,
    pub output: Option<String>,
    pub in_channel: usize,
    pub buffer_frames: Option<u32>,
    pub processing_sample_rate: Option<u32>,
    pub master_volume_db: Option<f32>,
    pub last_preset: Option<PresetRef>,
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let path = settings_path();
        if !path.exists() {
            return Self::default();
        }
        match fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Self>(&raw) {
                Ok(cfg) => cfg,
                Err(err) => {
                    eprintln!("Aviso: settings.json inválido ({}): {err}", path.display());
                    Self::default()
                }
            },
            Err(err) => {
                eprintln!("Aviso: no se pudo leer {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("No se pudo crear el directorio '{}'", parent.display())
            })?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&tmp, json)
            .with_context(|| format!("No se pudo escribir '{}'", tmp.display()))?;
        fs::rename(&tmp, &path)
            .with_context(|| format!("No se pudo actualizar '{}'", path.display()))?;
        Ok(())
    }
}

pub fn host_from_str(s: Option<&str>) -> Option<HostKind> {
    match s.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("wasapi") => Some(HostKind::Wasapi),
        Some("asio") => {
            #[cfg(feature = "asio")]
            {
                Some(HostKind::Asio)
            }
            #[cfg(not(feature = "asio"))]
            {
                Some(HostKind::Wasapi)
            }
        }
        _ => None,
    }
}

pub fn host_to_string(host: HostKind) -> String {
    match host {
        HostKind::Wasapi => "wasapi".to_string(),
        #[cfg(feature = "asio")]
        HostKind::Asio => "asio".to_string(),
    }
}
