use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use engine::Preset;

pub const FACTORY: &[(&str, &str)] = &[
    (
        "80s Chorus Clean",
        include_str!("../presets/chorus_clean.json"),
    ),
    ("Clean", include_str!("../presets/clean.json")),
    (
        "Clean Comp (Opto + Limiter)",
        include_str!("../presets/clean_comp.json"),
    ),
    ("Crunch", include_str!("../presets/crunch.json")),
    (
        "Funk Auto-Wah + 1x12",
        include_str!("../presets/funk_wah.json"),
    ),
    ("Lead (Boost + Drive)", include_str!("../presets/lead.json")),
    (
        "Surf Spring & Tremolo",
        include_str!("../presets/surf_spring.json"),
    ),
    (
        "Tube Stack (Boost + 12AX7)",
        include_str!("../presets/tube_stack.json"),
    ),
];

pub fn app_data_dir() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        PathBuf::from(appdata).join("libre-pedals")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".libre-pedals")
    } else {
        std::env::temp_dir().join("libre-pedals")
    }
}

pub fn user_presets_dir() -> Result<PathBuf> {
    let dir = app_data_dir().join("presets");
    fs::create_dir_all(&dir)
        .with_context(|| format!("No se pudo crear la carpeta de presets '{}'", dir.display()))?;
    Ok(dir)
}

pub fn user_irs_dir() -> Result<PathBuf> {
    let dir = app_data_dir().join("irs");
    fs::create_dir_all(&dir)
        .with_context(|| format!("No se pudo crear la carpeta de IRs '{}'", dir.display()))?;
    Ok(dir)
}

pub fn list_user_irs() -> Vec<(String, PathBuf)> {
    let Ok(dir) = user_irs_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
        {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("IR")
                .to_string();
            out.push((name, path));
        }
    }
    out.sort_by_key(|a| a.0.to_lowercase());
    out
}

pub fn sanitize_filename(name: &str) -> String {
    let mut out = String::new();
    for ch in name.trim().chars() {
        let mapped = if ch.is_control()
            || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        {
            '_'
        } else {
            ch
        };
        out.push(mapped);
        if out.chars().count() >= 64 {
            break;
        }
    }
    let trimmed = out.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        "preset".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn list_user_presets() -> Vec<(String, PathBuf)> {
    let Ok(dir) = user_presets_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_json = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(preset) = Preset::from_json(&raw) {
                let name = if preset.name.trim().is_empty() {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("preset")
                        .to_string()
                } else {
                    preset.name
                };
                out.push((name, path));
            }
        }
    }

    out.sort_by(|a, b| {
        a.0.to_lowercase()
            .cmp(&b.0.to_lowercase())
            .then_with(|| a.1.cmp(&b.1))
    });
    out
}

pub fn save_user_preset(preset: &Preset) -> Result<PathBuf> {
    let dir = user_presets_dir()?;
    let stem = sanitize_filename(&preset.name);
    let target = dir.join(format!("{stem}.json"));
    let tmp = dir.join(format!("{stem}.json.tmp"));
    let json = preset.to_json_pretty()?;
    fs::write(&tmp, json).with_context(|| format!("No se pudo escribir '{}'", tmp.display()))?;
    fs::rename(&tmp, &target)
        .with_context(|| format!("No se pudo guardar '{}'", target.display()))?;
    Ok(target)
}

pub fn delete_user_preset(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path)
            .with_context(|| format!("No se pudo eliminar '{}'", path.display()))?;
    }
    Ok(())
}

pub fn load_factory_preset(name: &str) -> Result<Preset> {
    for &(factory_name, json) in FACTORY {
        if factory_name.eq_ignore_ascii_case(name) {
            return Preset::from_json(json);
        }
    }
    bail!("Preset de fábrica desconocido: '{name}'.")
}

pub fn resolve_preset(name_or_path: &str) -> Result<Preset> {
    let as_path = Path::new(name_or_path);
    if as_path.is_file() {
        let raw = fs::read_to_string(as_path)
            .with_context(|| format!("No se pudo leer el preset '{}'", as_path.display()))?;
        return Preset::from_json(&raw);
    }

    for (user_name, path) in list_user_presets() {
        if user_name.eq_ignore_ascii_case(name_or_path) {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("No se pudo leer el preset '{}'", path.display()))?;
            return Preset::from_json(&raw);
        }
    }

    for &(factory_name, json) in FACTORY {
        if factory_name.eq_ignore_ascii_case(name_or_path) {
            return Preset::from_json(json);
        }
    }

    bail!("No se encontró el preset '{name_or_path}' ni como archivo, preset de usuario o de fábrica.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Params;

    #[test]
    fn factory_presets_parse_and_apply_without_warnings() {
        for &(name, json) in FACTORY {
            let preset = Preset::from_json(json).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(preset.name, name);
            let params = Params::default();
            let warnings = preset.apply(&params);
            assert!(
                warnings.is_empty(),
                "Preset de fábrica '{name}' produjo avisos: {warnings:?}"
            );
        }
    }

    #[test]
    fn sanitize_filename_strips_invalid_chars() {
        assert_eq!(sanitize_filename("Mi: Lead / Test*"), "Mi_ Lead _ Test_");
        assert_eq!(sanitize_filename("   ...  "), "preset");
    }
}
