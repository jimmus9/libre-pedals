use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::params::{
    Params, MAX_INSTANCES_PER_KIND, MAX_MOUNTED_PEDALS, PRESET_LEVEL_MAX_DB, PRESET_LEVEL_MIN_DB,
};

pub const PRESET_FORMAT_VERSION: u32 = 1;

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Preset {
    pub format_version: u32,
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub chain: Vec<PresetPedal>,
    #[serde(default)]
    pub preset_level_db: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PresetPedal {
    pub id: String,
    pub pedal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub params: BTreeMap<String, f32>,
}

impl Preset {
    /// Captura el estado actual de la pedalera (sin el Master Volume global).
    pub fn capture(params: &Params, name: impl Into<String>) -> Self {
        let mut chain = Vec::new();
        for slot_idx in params.mounted_cascade() {
            let slot = &params.slots()[slot_idx];
            if slot.is_master {
                continue;
            }
            let mut map = BTreeMap::new();
            for (p_idx, spec) in slot.specs.iter().enumerate() {
                map.insert(spec.id.to_string(), slot.get(p_idx));
            }
            let raw_label = slot.label();
            let label = if raw_label.is_empty() {
                None
            } else {
                Some(raw_label)
            };
            chain.push(PresetPedal {
                id: format!("{}-{}", slot.kind, slot.instance),
                pedal: slot.kind.to_string(),
                label,
                enabled: !slot.bypass(),
                params: map,
            });
        }

        Self {
            format_version: PRESET_FORMAT_VERSION,
            name: name.into(),
            author: String::new(),
            description: String::new(),
            chain,
            preset_level_db: params.preset_level_db(),
        }
    }

    /// Aplica el preset sobre `params` respetando límites e intacto el Master Volume.
    pub fn apply(&self, params: &Params) -> Vec<String> {
        let mut warnings = Vec::new();

        if !self.preset_level_db.is_finite() {
            warnings
                .push("Nivel de preset no válido (NaN/Inf); se restableció a 0.0 dB.".to_string());
            params.set_preset_level_db(0.0);
        } else {
            if self.preset_level_db < PRESET_LEVEL_MIN_DB
                || self.preset_level_db > PRESET_LEVEL_MAX_DB
            {
                let clamped = self
                    .preset_level_db
                    .clamp(PRESET_LEVEL_MIN_DB, PRESET_LEVEL_MAX_DB);
                warnings.push(format!(
                    "Nivel de preset {:.1} dB fuera de rango [{:.1}, {:.1}]; ajustado a {:.1} dB.",
                    self.preset_level_db, PRESET_LEVEL_MIN_DB, PRESET_LEVEL_MAX_DB, clamped
                ));
            }
            params.set_preset_level_db(self.preset_level_db);
        }

        let mut assigned = vec![false; params.slot_count()];
        let mut mounted_slots = Vec::new();

        for entry in &self.chain {
            if mounted_slots.len() >= MAX_MOUNTED_PEDALS {
                warnings.push(format!(
                    "Se alcanzó el límite máximo de {MAX_MOUNTED_PEDALS} pedales; se omitió '{}' ({}).",
                    entry.pedal, entry.id
                ));
                continue;
            }

            let kind_exists = params
                .slots()
                .iter()
                .any(|s| !s.is_master && s.kind == entry.pedal);
            if !kind_exists {
                warnings.push(format!(
                    "Pedal desconocido '{}' (id '{}'); se omitió.",
                    entry.pedal, entry.id
                ));
                continue;
            }

            let next_slot = params
                .slots()
                .iter()
                .enumerate()
                .filter(|(idx, s)| !s.is_master && s.kind == entry.pedal && !assigned[*idx])
                .min_by_key(|(_, s)| s.instance);

            let Some((slot_idx, slot)) = next_slot else {
                warnings.push(format!(
                    "Se superó el máximo de {MAX_INSTANCES_PER_KIND} instancias para '{}'; se omitió '{}'.",
                    entry.pedal, entry.id
                ));
                continue;
            };

            assigned[slot_idx] = true;
            slot.reset_defaults();
            slot.set_bypass(!entry.enabled);
            slot.set_label(entry.label.as_deref().unwrap_or(""));

            for (param_id, &val) in &entry.params {
                let Some((p_idx, spec)) = slot
                    .specs
                    .iter()
                    .enumerate()
                    .find(|(_, sp)| sp.id == param_id)
                else {
                    warnings.push(format!(
                        "Parámetro desconocido '{param_id}' en '{}' ({}); se ignoró.",
                        entry.pedal, entry.id
                    ));
                    continue;
                };

                if !val.is_finite() {
                    warnings.push(format!(
                        "Valor no finito para '{param_id}' en '{}'; se usó {:.2}.",
                        entry.pedal, spec.default
                    ));
                    slot.set(p_idx, spec.default);
                    continue;
                }

                if val < spec.min || val > spec.max {
                    let clamped = spec.clamp(val);
                    warnings.push(format!(
                        "Valor {val} fuera de rango [{}, {}] para '{param_id}' en '{}'; ajustado a {clamped}.",
                        spec.min, spec.max, entry.pedal
                    ));
                }
                slot.set(p_idx, val);
            }

            mounted_slots.push(slot_idx);
        }

        params.set_cascade(&mounted_slots);
        warnings
    }

    pub fn to_json_pretty(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn from_json(raw: &str) -> anyhow::Result<Self> {
        let preset: Self = serde_json::from_str(raw)?;
        if preset.format_version == 0 || preset.format_version > PRESET_FORMAT_VERSION {
            anyhow::bail!(
                "Versión de formato de preset no soportada: {} (máxima soportada: {}).",
                preset.format_version,
                PRESET_FORMAT_VERSION
            );
        }
        Ok(preset)
    }

    /// Compara el sonido efectivo del preset (nivel, pedales, orden, estado, etiquetas y parámetros),
    /// ignorando metadatos como `id` o `name`.
    pub fn same_sound(&self, other: &Self) -> bool {
        if (self.preset_level_db - other.preset_level_db).abs() > 1e-4 {
            return false;
        }
        if self.chain.len() != other.chain.len() {
            return false;
        }
        for (a, b) in self.chain.iter().zip(other.chain.iter()) {
            if a.pedal != b.pedal || a.enabled != b.enabled {
                return false;
            }
            let la = a.label.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let lb = b.label.as_deref().map(str::trim).filter(|s| !s.is_empty());
            if la != lb {
                return false;
            }
            if a.params.len() != b.params.len() {
                return false;
            }
            for (k, va) in &a.params {
                match b.params.get(k) {
                    Some(vb) if (va - vb).abs() <= 1e-4 => {}
                    _ => return false,
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_capture_json_apply_preserves_sound() {
        let params = Params::default();
        params.set_gate_threshold_db(-52.5);
        params.set_drive_db(21.0);
        params.set_tone_hz(3_200.0);
        params.set_level_db(-4.5);
        params.set_preset_level_db(1.5);
        params.set_volume_db(-9.0);

        let captured = Preset::capture(&params, "Mi Crunch");
        let json = captured.to_json_pretty().expect("serialize");
        let parsed = Preset::from_json(&json).expect("deserialize");

        let target = Params::default();
        target.set_volume_db(-15.0);
        let warnings = parsed.apply(&target);
        assert!(warnings.is_empty(), "warnings inesperados: {warnings:?}");

        // Master no debe cambiar al aplicar el preset y debe permanecer al final.
        assert!((target.volume_db() - (-15.0)).abs() < 1e-5);
        assert!(target.slots()[target.order_at(target.slot_count() - 1)].is_master);

        let recaptured = Preset::capture(&target, "Otro nombre");
        assert!(captured.same_sound(&recaptured));
    }

    #[test]
    fn two_overdrives_mount_in_order_and_five_overdrives_warns() {
        let mut two = Preset {
            format_version: PRESET_FORMAT_VERSION,
            name: "Stack".to_string(),
            author: String::new(),
            description: String::new(),
            chain: vec![
                PresetPedal {
                    id: "od-1".to_string(),
                    pedal: "overdrive".to_string(),
                    label: Some("Boost".to_string()),
                    enabled: true,
                    params: BTreeMap::from([("drive_db".to_string(), 4.0)]),
                },
                PresetPedal {
                    id: "od-2".to_string(),
                    pedal: "overdrive".to_string(),
                    label: Some("Drive".to_string()),
                    enabled: false,
                    params: BTreeMap::from([("drive_db".to_string(), 28.0)]),
                },
            ],
            preset_level_db: 0.0,
        };

        let params = Params::default();
        let w = two.apply(&params);
        assert!(w.is_empty(), "warnings inesperados: {w:?}");
        let cascade = params.mounted_cascade();
        assert_eq!(cascade.len(), 2);
        assert_eq!(params.slots()[cascade[0]].instance, 1);
        assert_eq!(params.slots()[cascade[0]].label(), "Boost");
        assert_eq!(params.slots()[cascade[1]].instance, 2);
        assert_eq!(params.slots()[cascade[1]].label(), "Drive");
        assert!(params.slots()[cascade[1]].bypass());

        // 5 overdrives -> monta 4 y emite aviso por el 5º.
        two.chain = (1..=5)
            .map(|i| PresetPedal {
                id: format!("od-{i}"),
                pedal: "overdrive".to_string(),
                label: None,
                enabled: true,
                params: BTreeMap::new(),
            })
            .collect();
        let warnings = two.apply(&params);
        assert_eq!(params.mounted_cascade().len(), MAX_INSTANCES_PER_KIND);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn unknown_param_and_out_of_range_values_warn_and_clamp() {
        let preset = Preset {
            format_version: PRESET_FORMAT_VERSION,
            name: "Limites".to_string(),
            author: String::new(),
            description: String::new(),
            chain: vec![PresetPedal {
                id: "od-1".to_string(),
                pedal: "overdrive".to_string(),
                label: None,
                enabled: true,
                params: BTreeMap::from([
                    ("drive_db".to_string(), 99.0),
                    ("param_inexistente".to_string(), 1.0),
                ]),
            }],
            preset_level_db: 0.0,
        };

        let params = Params::default();
        let warnings = preset.apply(&params);
        assert_eq!(warnings.len(), 2, "warnings: {warnings:?}");
        assert!((params.drive_db() - 40.0).abs() < 1e-5);
    }

    #[test]
    fn unsupported_format_version_returns_error() {
        let json = r#"{
            "format_version": 99,
            "name": "Futuro",
            "chain": []
        }"#;
        let err = Preset::from_json(json).expect_err("debe fallar versión futura");
        assert!(err.to_string().contains("99"));
    }
}
