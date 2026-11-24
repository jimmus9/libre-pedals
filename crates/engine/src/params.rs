use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use dsp::{lin_to_db, Chain, ParamSpec};

pub const MAX_INSTANCES_PER_KIND: usize = 4;
pub const MAX_MOUNTED_PEDALS: usize = 12;
pub const PRESET_LEVEL_MIN_DB: f32 = -24.0;
pub const PRESET_LEVEL_MAX_DB: f32 = 12.0;

/// Resumen de disponibilidad por tipo de pedal en la cadena prealojada.
#[derive(Debug, Clone)]
pub struct KindInfo {
    pub kind: &'static str,
    pub name: &'static str,
    pub accent_color: [u8; 3],
    pub used: usize,
    pub max: usize,
}

/// Contenedor atómico lock-free para valores `f32` compartidos con el hilo de audio.
#[derive(Debug)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(val: f32) -> Self {
        Self(AtomicU32::new(val.to_bits()))
    }

    #[inline]
    pub fn load(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn store(&self, val: f32) {
        self.0.store(val.to_bits(), Ordering::Relaxed);
    }

    /// Actualiza el máximo retenido de forma atómica (sin bloqueos) desde el hilo de audio.
    #[inline]
    pub fn fetch_max_abs(&self, val: f32) {
        let abs = if val.is_finite() { val.abs() } else { 0.0 };
        let mut cur_bits = self.0.load(Ordering::Relaxed);
        loop {
            let cur = f32::from_bits(cur_bits);
            if abs <= cur {
                return;
            }
            match self.0.compare_exchange_weak(
                cur_bits,
                abs.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(actual) => cur_bits = actual,
            }
        }
    }

    /// Lee y reinicia el pico retenido desde el hilo de UI/CLI.
    #[inline]
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.0.swap(0.0_f32.to_bits(), Ordering::Relaxed))
    }
}

/// Configuración inicial de parámetros de la cadena de efectos por defecto.
#[derive(Debug, Clone, Copy)]
pub struct InitialParams {
    pub drive_db: f32,
    pub tone_hz: f32,
    pub level_db: f32,
    pub gate_threshold_db: f32,
    pub volume_db: f32,
    pub mute: bool,
    pub bypass_gate: bool,
    pub bypass_drive: bool,
    pub bypass_volume: bool,
}

impl Default for InitialParams {
    fn default() -> Self {
        Self {
            drive_db: 12.0,
            tone_hz: 4_000.0,
            level_db: -6.0,
            gate_threshold_db: -60.0,
            volume_db: -6.0,
            mute: false,
            bypass_gate: false,
            bypass_drive: false,
            bypass_volume: false,
        }
    }
}

/// Banco de parámetros atómicos autodescriptivos para una instancia individual de pedal.
#[derive(Debug)]
pub struct PedalParamSlot {
    pub kind: &'static str,
    pub name: &'static str,
    pub instance: usize,
    pub accent_color: [u8; 3],
    pub specs: &'static [ParamSpec],
    pub is_master: bool,
    values: Box<[AtomicF32]>,
    bypass: AtomicBool,
    mounted: AtomicBool,
    label: Mutex<String>,
}

impl PedalParamSlot {
    #[inline]
    pub fn param_count(&self) -> usize {
        self.specs.len()
    }

    #[inline]
    pub fn get(&self, param_idx: usize) -> f32 {
        self.values.get(param_idx).map_or(0.0, |atom| atom.load())
    }

    #[inline]
    pub fn set(&self, param_idx: usize, val: f32) {
        if let (Some(spec), Some(atom)) = (self.specs.get(param_idx), self.values.get(param_idx)) {
            atom.store(spec.clamp(val));
        }
    }

    pub fn reset_defaults(&self) {
        for (spec, atom) in self.specs.iter().zip(self.values.iter()) {
            atom.store(spec.default);
        }
    }

    #[inline]
    pub fn bypass(&self) -> bool {
        self.bypass.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_bypass(&self, bypass: bool) {
        self.bypass.store(bypass, Ordering::Relaxed);
    }

    #[inline]
    pub fn is_mounted(&self) -> bool {
        self.mounted.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn set_mounted(&self, mounted: bool) {
        self.mounted.store(mounted, Ordering::Relaxed);
    }

    pub fn label(&self) -> String {
        self.label.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_label(&self, s: &str) {
        let cleaned: String = s.trim().chars().take(24).collect();
        *self.label.lock().unwrap_or_else(|e| e.into_inner()) = cleaned;
    }

    pub fn display_name(&self) -> String {
        let label = self.label();
        if !label.is_empty() {
            label
        } else if self.instance <= 1 {
            self.name.to_string()
        } else {
            format!("{} ({})", self.name, self.instance)
        }
    }
}

/// Parámetros compartidos entre la UI/CLI y el callback de audio mediante atómicos.
#[derive(Debug)]
pub struct Params {
    slots: Box<[PedalParamSlot]>,
    order: Box<[AtomicUsize]>,
    mute: AtomicBool,
    preset_level_db: AtomicF32,
}

impl Default for Params {
    fn default() -> Self {
        Self::from_initial(InitialParams::default())
    }
}

impl Params {
    /// Construye el banco reactivo a partir de cualquier `dsp::Chain`.
    pub fn from_chain(chain: &Chain, mute: bool) -> Self {
        let mut slots = Vec::with_capacity(chain.len());
        let mut order = Vec::with_capacity(chain.len());
        let mut kind_counts: HashMap<&'static str, usize> = HashMap::new();

        for idx in 0..chain.len() {
            if let Some((fx, bypass)) = chain.slot(idx) {
                let kind = fx.kind();
                let count = kind_counts.entry(kind).or_insert(0);
                *count += 1;
                let instance = *count;
                let is_master = kind == "master_volume";
                let specs = fx.params();
                let values: Vec<AtomicF32> = (0..specs.len())
                    .map(|p_idx| AtomicF32::new(specs[p_idx].clamp(fx.get_param(p_idx))))
                    .collect();
                slots.push(PedalParamSlot {
                    kind,
                    name: fx.name(),
                    instance,
                    accent_color: fx.accent_color(),
                    specs,
                    is_master,
                    values: values.into_boxed_slice(),
                    bypass: AtomicBool::new(*bypass),
                    mounted: AtomicBool::new(instance == 1),
                    label: Mutex::new(String::new()),
                });
                order.push(AtomicUsize::new(idx));
            }
        }

        let params = Self {
            slots: slots.into_boxed_slice(),
            order: order.into_boxed_slice(),
            mute: AtomicBool::new(mute),
            preset_level_db: AtomicF32::new(0.0),
        };
        let initial_mounted: Vec<usize> = params
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.is_master && s.is_mounted())
            .map(|(idx, _)| idx)
            .collect();
        params.set_cascade(&initial_mounted);
        params
    }

    pub fn from_initial(init: InitialParams) -> Self {
        let params = Self::from_chain(&crate::default_pedalboard_chain(), init.mute);
        params.set_gate_threshold_db(init.gate_threshold_db);
        params.set_bypass_gate(init.bypass_gate);
        params.set_drive_db(init.drive_db);
        params.set_tone_hz(init.tone_hz);
        params.set_level_db(init.level_db);
        params.set_bypass_drive(init.bypass_drive);
        params.set_volume_db(init.volume_db);
        params.set_bypass_volume(init.bypass_volume);
        params
    }

    #[inline]
    pub fn slots(&self) -> &[PedalParamSlot] {
        &self.slots
    }

    #[inline]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    #[inline]
    pub fn order_at(&self, cascade_pos: usize) -> usize {
        self.order
            .get(cascade_pos)
            .map_or(cascade_pos, |a| a.load(Ordering::Relaxed))
    }

    pub fn swap_cascade_positions(&self, pos_a: usize, pos_b: usize) {
        if pos_a == pos_b || pos_a >= self.order.len() || pos_b >= self.order.len() {
            return;
        }
        let val_a = self.order[pos_a].load(Ordering::Acquire);
        let val_b = self.order[pos_b].load(Ordering::Acquire);
        self.order[pos_a].store(val_b, Ordering::Release);
        self.order[pos_b].store(val_a, Ordering::Release);
    }

    /// Devuelve el slot de volumen maestro fijo (si existe).
    pub fn master_slot(&self) -> Option<&PedalParamSlot> {
        self.slots.iter().find(|s| s.is_master)
    }

    /// Primer slot del tipo indicado (menor número de instancia).
    pub fn first_slot_of_kind(&self, kind: &str) -> Option<(usize, &PedalParamSlot)> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == kind)
            .min_by_key(|(_, s)| s.instance)
    }

    /// Lista los tipos de pedal disponibles (sin el volumen maestro) en orden de aparición.
    pub fn kinds(&self) -> Vec<KindInfo> {
        let mut order: Vec<&'static str> = Vec::new();
        let mut by_kind: BTreeMap<&'static str, KindInfo> = BTreeMap::new();

        for slot in self.slots.iter() {
            if slot.is_master {
                continue;
            }
            let entry = by_kind.entry(slot.kind).or_insert_with(|| {
                order.push(slot.kind);
                KindInfo {
                    kind: slot.kind,
                    name: slot.name,
                    accent_color: slot.accent_color,
                    used: 0,
                    max: 0,
                }
            });
            entry.max += 1;
            if slot.is_mounted() {
                entry.used += 1;
            }
        }

        order
            .into_iter()
            .filter_map(|k| by_kind.remove(k))
            .collect()
    }

    /// Monta una nueva instancia libre de `kind` al final de la cascada (antes del Master).
    pub fn mount_new_instance(&self, kind: &str) -> Option<usize> {
        let total_mounted = self
            .slots
            .iter()
            .filter(|s| !s.is_master && s.is_mounted())
            .count();
        if total_mounted >= MAX_MOUNTED_PEDALS {
            return None;
        }
        let (idx, slot) = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == kind && !s.is_master && !s.is_mounted())
            .min_by_key(|(_, s)| s.instance)?;

        slot.reset_defaults();
        slot.set_bypass(false);
        slot.set_label("");
        self.move_slot_to_end(idx);
        Some(idx)
    }

    /// Índices de los slots montados (sin Master) en su orden actual de cascada.
    pub fn mounted_cascade(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.slots.len());
        for pos in 0..self.order.len() {
            let idx = self.order_at(pos);
            if let Some(slot) = self.slots.get(idx) {
                if !slot.is_master && slot.is_mounted() {
                    out.push(idx);
                }
            }
        }
        out
    }

    /// Fija qué slots están montados y su orden exacto, dejando los restantes desmontados y el Master al final.
    pub fn set_cascade(&self, mounted: &[usize]) {
        for slot in self.slots.iter() {
            if !slot.is_master {
                slot.set_mounted(false);
            }
        }

        let mut seen = vec![false; self.slots.len()];
        let mut new_order = Vec::with_capacity(self.slots.len());

        for &idx in mounted {
            if let Some(slot) = self.slots.get(idx) {
                if !slot.is_master && !seen[idx] {
                    seen[idx] = true;
                    slot.set_mounted(true);
                    new_order.push(idx);
                }
            }
        }

        for (idx, slot) in self.slots.iter().enumerate() {
            if !slot.is_master && !seen[idx] {
                seen[idx] = true;
                new_order.push(idx);
            }
        }

        for (idx, slot) in self.slots.iter().enumerate() {
            if slot.is_master && !seen[idx] {
                seen[idx] = true;
                new_order.push(idx);
            }
        }

        for (pos, idx) in new_order.into_iter().enumerate() {
            if let Some(atom) = self.order.get(pos) {
                atom.store(idx, Ordering::Release);
            }
        }
    }

    /// Monta un pedal y lo ubica al final de los pedales de efecto (antes del Master Volume).
    pub fn move_slot_to_end(&self, slot_idx: usize) {
        if slot_idx >= self.slots.len() || self.slots[slot_idx].is_master {
            return;
        }
        let mut effects: Vec<usize> = Vec::with_capacity(self.order.len());
        let mut unmounted: Vec<usize> = Vec::with_capacity(self.order.len());
        let mut masters: Vec<usize> = Vec::with_capacity(1);
        for atom in self.order.iter() {
            let idx = atom.load(Ordering::Acquire);
            if idx == slot_idx {
                continue;
            }
            if let Some(slot) = self.slots.get(idx) {
                if slot.is_master {
                    masters.push(idx);
                } else if slot.is_mounted() {
                    effects.push(idx);
                } else {
                    unmounted.push(idx);
                }
            }
        }
        effects.push(slot_idx);
        effects.extend(unmounted);
        effects.extend(masters);
        for (pos, idx) in effects.into_iter().enumerate() {
            if let Some(atom) = self.order.get(pos) {
                atom.store(idx, Ordering::Release);
            }
        }
        self.slots[slot_idx].set_mounted(true);
    }

    #[inline]
    pub fn preset_level_db(&self) -> f32 {
        self.preset_level_db.load()
    }

    #[inline]
    pub fn set_preset_level_db(&self, db: f32) {
        let clamped = if db.is_finite() {
            db.clamp(PRESET_LEVEL_MIN_DB, PRESET_LEVEL_MAX_DB)
        } else {
            0.0
        };
        self.preset_level_db.store(clamped);
    }

    pub fn set_drive_db(&self, db: f32) {
        if let Some((_, s)) = self.first_slot_of_kind("overdrive") {
            s.set(0, db);
        }
    }
    pub fn drive_db(&self) -> f32 {
        self.first_slot_of_kind("overdrive")
            .map_or(12.0, |(_, s)| s.get(0))
    }

    pub fn set_tone_hz(&self, hz: f32) {
        if let Some((_, s)) = self.first_slot_of_kind("overdrive") {
            s.set(1, hz);
        }
    }
    pub fn tone_hz(&self) -> f32 {
        self.first_slot_of_kind("overdrive")
            .map_or(4_000.0, |(_, s)| s.get(1))
    }

    pub fn set_level_db(&self, db: f32) {
        if let Some((_, s)) = self.first_slot_of_kind("overdrive") {
            s.set(2, db);
        }
    }
    pub fn level_db(&self) -> f32 {
        self.first_slot_of_kind("overdrive")
            .map_or(-6.0, |(_, s)| s.get(2))
    }

    pub fn set_gate_threshold_db(&self, db: f32) {
        if let Some((_, s)) = self.first_slot_of_kind("noise_gate") {
            s.set(0, db);
        }
    }
    pub fn gate_threshold_db(&self) -> f32 {
        self.first_slot_of_kind("noise_gate")
            .map_or(-60.0, |(_, s)| s.get(0))
    }

    pub fn set_volume_db(&self, db: f32) {
        if let Some((_, s)) = self.first_slot_of_kind("master_volume") {
            s.set(0, db);
        }
    }
    pub fn volume_db(&self) -> f32 {
        self.first_slot_of_kind("master_volume")
            .map_or(-6.0, |(_, s)| s.get(0))
    }

    pub fn set_mute(&self, mute: bool) {
        self.mute.store(mute, Ordering::Relaxed);
    }
    pub fn mute(&self) -> bool {
        self.mute.load(Ordering::Relaxed)
    }

    pub fn set_bypass_gate(&self, bypass: bool) {
        if let Some((_, s)) = self.first_slot_of_kind("noise_gate") {
            s.set_bypass(bypass);
        }
    }
    pub fn bypass_gate(&self) -> bool {
        self.first_slot_of_kind("noise_gate")
            .is_some_and(|(_, s)| s.bypass())
    }

    pub fn set_bypass_drive(&self, bypass: bool) {
        if let Some((_, s)) = self.first_slot_of_kind("overdrive") {
            s.set_bypass(bypass);
        }
    }
    pub fn bypass_drive(&self) -> bool {
        self.first_slot_of_kind("overdrive")
            .is_some_and(|(_, s)| s.bypass())
    }

    pub fn set_bypass_volume(&self, bypass: bool) {
        if let Some((_, s)) = self.first_slot_of_kind("master_volume") {
            s.set_bypass(bypass);
        }
    }
    pub fn bypass_volume(&self) -> bool {
        self.first_slot_of_kind("master_volume")
            .is_some_and(|(_, s)| s.bypass())
    }
}

/// Medidores en tiempo real (picos de entrada/salida y contadores de cortes).
#[derive(Debug)]
pub struct Meters {
    pub(crate) input_peak: AtomicF32,
    pub(crate) output_peak: AtomicF32,
    pub(crate) underruns: AtomicU64,
    pub(crate) overflows: AtomicU64,
    pub(crate) drift_drops: AtomicU64,
    pub(crate) stream_errors: AtomicU64,
}

impl Default for Meters {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MeterSnapshot {
    pub input_peak: f32,
    pub output_peak: f32,
    pub input_db: f32,
    pub output_db: f32,
    pub underruns: u64,
    pub overflows: u64,
    pub drift_drops: u64,
    pub stream_errors: u64,
}

impl Meters {
    pub fn new() -> Self {
        Self {
            input_peak: AtomicF32::new(0.0),
            output_peak: AtomicF32::new(0.0),
            underruns: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            drift_drops: AtomicU64::new(0),
            stream_errors: AtomicU64::new(0),
        }
    }

    /// Toma una lectura de los medidores y reinicia los picos retenidos.
    pub fn snapshot_and_reset_peaks(&self) -> MeterSnapshot {
        let input_peak = self.input_peak.take_peak();
        let output_peak = self.output_peak.take_peak();
        MeterSnapshot {
            input_peak,
            output_peak,
            input_db: lin_to_db(input_peak),
            output_db: lin_to_db(output_peak),
            underruns: self.underruns.load(Ordering::Relaxed),
            overflows: self.overflows.load(Ordering::Relaxed),
            drift_drops: self.drift_drops.load(Ordering::Relaxed),
            stream_errors: self.stream_errors.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_mounting_and_master_separation_keep_master_last() {
        let params = Params::default();
        assert_eq!(params.slot_count(), 2 * MAX_INSTANCES_PER_KIND + 1);
        let master = params.master_slot().expect("master slot");
        assert_eq!(master.name, "Master Volume");
        assert_eq!(master.kind, "master_volume");

        let cascade = params.mounted_cascade();
        assert_eq!(cascade.len(), 2);
        let gate_idx = cascade[0];
        let od_idx = cascade[1];
        assert_eq!(params.slots()[gate_idx].kind, "noise_gate");
        assert_eq!(params.slots()[od_idx].kind, "overdrive");

        // Desmontar Noise Gate y volverlo a montar al final -> debe quedar después del Overdrive y antes del Master.
        params.slots()[gate_idx].set_mounted(false);
        assert!(!params.slots()[gate_idx].is_mounted());
        params.move_slot_to_end(gate_idx);
        assert!(params.slots()[gate_idx].is_mounted());

        let new_cascade = params.mounted_cascade();
        assert_eq!(new_cascade, vec![od_idx, gate_idx]);
        let last_slot_idx = params.order_at(params.slot_count() - 1);
        assert!(params.slots()[last_slot_idx].is_master);
    }

    #[test]
    fn mount_new_instance_respects_limit_and_set_cascade_keeps_master_last() {
        let params = Params::default();
        // Ya hay 1 overdrive montado (instance 1); podemos montar 3 más hasta 4.
        let mut mounted_ods = vec![params.first_slot_of_kind("overdrive").unwrap().0];
        for expected_inst in 2..=MAX_INSTANCES_PER_KIND {
            let idx = params
                .mount_new_instance("overdrive")
                .expect("debe haber instancia libre");
            assert_eq!(params.slots()[idx].instance, expected_inst);
            mounted_ods.push(idx);
        }
        assert!(params.mount_new_instance("overdrive").is_none());

        // Invertimos el orden de dos overdrives y verificamos que Master sigue último.
        let reordered = vec![mounted_ods[2], mounted_ods[0]];
        params.set_cascade(&reordered);
        assert_eq!(params.mounted_cascade(), reordered);
        let last_idx = params.order_at(params.slot_count() - 1);
        assert!(params.slots()[last_idx].is_master);
    }
}
