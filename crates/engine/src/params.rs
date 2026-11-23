use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use dsp::lin_to_db;

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

/// Configuración inicial de parámetros de la cadena de efectos.
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

/// Parámetros compartidos entre la aplicación y el callback de audio mediante atómicos.
#[derive(Debug)]
pub struct Params {
    drive_db: AtomicF32,
    tone_hz: AtomicF32,
    level_db: AtomicF32,
    gate_threshold_db: AtomicF32,
    volume_db: AtomicF32,
    mute: AtomicBool,
    bypass_gate: AtomicBool,
    bypass_drive: AtomicBool,
    bypass_volume: AtomicBool,
}

impl Default for Params {
    fn default() -> Self {
        Self::from_initial(InitialParams::default())
    }
}

impl Params {
    pub fn from_initial(init: InitialParams) -> Self {
        Self {
            drive_db: AtomicF32::new(init.drive_db),
            tone_hz: AtomicF32::new(init.tone_hz),
            level_db: AtomicF32::new(init.level_db),
            gate_threshold_db: AtomicF32::new(init.gate_threshold_db),
            volume_db: AtomicF32::new(init.volume_db),
            mute: AtomicBool::new(init.mute),
            bypass_gate: AtomicBool::new(init.bypass_gate),
            bypass_drive: AtomicBool::new(init.bypass_drive),
            bypass_volume: AtomicBool::new(init.bypass_volume),
        }
    }

    pub fn set_drive_db(&self, db: f32) {
        self.drive_db.store(db);
    }
    pub fn drive_db(&self) -> f32 {
        self.drive_db.load()
    }

    pub fn set_tone_hz(&self, hz: f32) {
        self.tone_hz.store(hz);
    }
    pub fn tone_hz(&self) -> f32 {
        self.tone_hz.load()
    }

    pub fn set_level_db(&self, db: f32) {
        self.level_db.store(db);
    }
    pub fn level_db(&self) -> f32 {
        self.level_db.load()
    }

    pub fn set_gate_threshold_db(&self, db: f32) {
        self.gate_threshold_db.store(db);
    }
    pub fn gate_threshold_db(&self) -> f32 {
        self.gate_threshold_db.load()
    }

    pub fn set_volume_db(&self, db: f32) {
        self.volume_db.store(db);
    }
    pub fn volume_db(&self) -> f32 {
        self.volume_db.load()
    }

    pub fn set_mute(&self, mute: bool) {
        self.mute.store(mute, Ordering::Relaxed);
    }
    pub fn mute(&self) -> bool {
        self.mute.load(Ordering::Relaxed)
    }

    pub fn set_bypass_gate(&self, bypass: bool) {
        self.bypass_gate.store(bypass, Ordering::Relaxed);
    }
    pub fn bypass_gate(&self) -> bool {
        self.bypass_gate.load(Ordering::Relaxed)
    }

    pub fn set_bypass_drive(&self, bypass: bool) {
        self.bypass_drive.store(bypass, Ordering::Relaxed);
    }
    pub fn bypass_drive(&self) -> bool {
        self.bypass_drive.load(Ordering::Relaxed)
    }

    pub fn set_bypass_volume(&self, bypass: bool) {
        self.bypass_volume.store(bypass, Ordering::Relaxed);
    }
    pub fn bypass_volume(&self) -> bool {
        self.bypass_volume.load(Ordering::Relaxed)
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
