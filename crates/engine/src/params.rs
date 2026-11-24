use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use dsp::{lin_to_db, Chain, ParamSpec};

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

/// Banco de parámetros atómicos autodescriptivos para un pedal individual.
#[derive(Debug)]
pub struct PedalParamSlot {
    pub name: &'static str,
    pub accent_color: [u8; 3],
    pub specs: &'static [ParamSpec],
    pub is_master: bool,
    values: Box<[AtomicF32]>,
    bypass: AtomicBool,
    mounted: AtomicBool,
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
}

/// Parámetros compartidos entre la UI/CLI y el callback de audio mediante atómicos.
#[derive(Debug)]
pub struct Params {
    slots: Box<[PedalParamSlot]>,
    order: Box<[AtomicUsize]>,
    mute: AtomicBool,
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

        for idx in 0..chain.len() {
            if let Some((fx, bypass)) = chain.slot(idx) {
                let specs = fx.params();
                let values: Vec<AtomicF32> = (0..specs.len())
                    .map(|p_idx| AtomicF32::new(specs[p_idx].clamp(fx.get_param(p_idx))))
                    .collect();
                slots.push(PedalParamSlot {
                    name: fx.name(),
                    accent_color: fx.accent_color(),
                    specs,
                    is_master: fx.name() == "Master Volume",
                    values: values.into_boxed_slice(),
                    bypass: AtomicBool::new(*bypass),
                    mounted: AtomicBool::new(true),
                });
                order.push(AtomicUsize::new(idx));
            }
        }

        Self {
            slots: slots.into_boxed_slice(),
            order: order.into_boxed_slice(),
            mute: AtomicBool::new(mute),
        }
    }

    pub fn from_initial(init: InitialParams) -> Self {
        let mut chain = crate::default_pedalboard_chain();
        if let Some((fx, bypass)) = chain.slot_mut(0) {
            fx.set_param(0, init.gate_threshold_db);
            *bypass = init.bypass_gate;
        }
        if let Some((fx, bypass)) = chain.slot_mut(1) {
            fx.set_param(0, init.drive_db);
            fx.set_param(1, init.tone_hz);
            fx.set_param(2, init.level_db);
            *bypass = init.bypass_drive;
        }
        if let Some((fx, bypass)) = chain.slot_mut(2) {
            fx.set_param(0, init.volume_db);
            *bypass = init.bypass_volume;
        }
        Self::from_chain(&chain, init.mute)
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

    /// Monta un pedal y lo ubica al final de los pedales de efecto (antes del Master Volume).
    pub fn move_slot_to_end(&self, slot_idx: usize) {
        if slot_idx >= self.slots.len() || self.slots[slot_idx].is_master {
            return;
        }
        let mut effects: Vec<usize> = Vec::with_capacity(self.order.len());
        let mut masters: Vec<usize> = Vec::with_capacity(1);
        for atom in self.order.iter() {
            let idx = atom.load(Ordering::Acquire);
            if idx == slot_idx {
                continue;
            }
            if self.slots.get(idx).is_some_and(|s| s.is_master) {
                masters.push(idx);
            } else {
                effects.push(idx);
            }
        }
        effects.push(slot_idx);
        effects.extend(masters);
        for (pos, idx) in effects.into_iter().enumerate() {
            if let Some(atom) = self.order.get(pos) {
                atom.store(idx, Ordering::Release);
            }
        }
        self.slots[slot_idx].set_mounted(true);
    }

    pub fn set_drive_db(&self, db: f32) {
        if let Some(s) = self.slots.get(1) {
            s.set(0, db);
        }
    }
    pub fn drive_db(&self) -> f32 {
        self.slots.get(1).map_or(12.0, |s| s.get(0))
    }

    pub fn set_tone_hz(&self, hz: f32) {
        if let Some(s) = self.slots.get(1) {
            s.set(1, hz);
        }
    }
    pub fn tone_hz(&self) -> f32 {
        self.slots.get(1).map_or(4_000.0, |s| s.get(1))
    }

    pub fn set_level_db(&self, db: f32) {
        if let Some(s) = self.slots.get(1) {
            s.set(2, db);
        }
    }
    pub fn level_db(&self) -> f32 {
        self.slots.get(1).map_or(-6.0, |s| s.get(2))
    }

    pub fn set_gate_threshold_db(&self, db: f32) {
        if let Some(s) = self.slots.first() {
            s.set(0, db);
        }
    }
    pub fn gate_threshold_db(&self) -> f32 {
        self.slots.first().map_or(-60.0, |s| s.get(0))
    }

    pub fn set_volume_db(&self, db: f32) {
        if let Some(s) = self.slots.get(2) {
            s.set(0, db);
        }
    }
    pub fn volume_db(&self) -> f32 {
        self.slots.get(2).map_or(-6.0, |s| s.get(0))
    }

    pub fn set_mute(&self, mute: bool) {
        self.mute.store(mute, Ordering::Relaxed);
    }
    pub fn mute(&self) -> bool {
        self.mute.load(Ordering::Relaxed)
    }

    pub fn set_bypass_gate(&self, bypass: bool) {
        if let Some(s) = self.slots.first() {
            s.set_bypass(bypass);
        }
    }
    pub fn bypass_gate(&self) -> bool {
        self.slots.first().is_some_and(|s| s.bypass())
    }

    pub fn set_bypass_drive(&self, bypass: bool) {
        if let Some(s) = self.slots.get(1) {
            s.set_bypass(bypass);
        }
    }
    pub fn bypass_drive(&self) -> bool {
        self.slots.get(1).is_some_and(|s| s.bypass())
    }

    pub fn set_bypass_volume(&self, bypass: bool) {
        if let Some(s) = self.slots.get(2) {
            s.set_bypass(bypass);
        }
    }
    pub fn bypass_volume(&self) -> bool {
        self.slots.get(2).is_some_and(|s| s.bypass())
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
        assert_eq!(params.slot_count(), 3);
        assert!(params.master_slot().is_some());
        assert_eq!(params.master_slot().unwrap().name, "Master Volume");
        assert!(!params.slots()[0].is_master);
        assert!(!params.slots()[1].is_master);

        // Desmontar Noise Gate (0) y volverlo a montar al final -> debe quedar antes de Master (2).
        params.slots()[0].set_mounted(false);
        assert!(!params.slots()[0].is_mounted());
        params.move_slot_to_end(0);
        assert!(params.slots()[0].is_mounted());
        assert_eq!(params.order_at(0), 1); // Overdrive
        assert_eq!(params.order_at(1), 0); // Noise Gate
        assert_eq!(params.order_at(2), 2); // Master Volume siempre al final
    }
}
