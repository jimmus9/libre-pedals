//! Protección contra números subnormales (*denormals*) en el hilo de audio en tiempo real.
//!
//! En CPUs x86/x86_64, los decaimientos largos de filtros IIR o reverbs pueden caer en el rango
//! subnormal (`|x| < 1.175e-38` en `f32`) y disparar penalizaciones por microcódigo de 10–100x.
//! [`ScopedNoDenormals`] activa los bits hardware **FTZ** (*Flush-To-Zero*) y **DAZ**
//! (*Denormals-Are-Zero*) en el registro `MXCSR` durante la vida del guard RAII y restaura el
//! estado previo al salir de ámbito.

/// Guard RAII que activa FTZ/DAZ durante el procesado de un bloque de audio y restaura el estado al soltarlo.
pub struct ScopedNoDenormals {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    prev_mxcsr: u32,
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const MXCSR_FTZ_BIT: u32 = 1 << 15;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const MXCSR_DAZ_BIT: u32 = 1 << 6;

impl ScopedNoDenormals {
    /// Activa FTZ y DAZ en el hilo actual (no-op seguro en arquitecturas sin MXCSR).
    #[inline]
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            #[allow(deprecated)]
            let prev = core::arch::x86_64::_mm_getcsr();
            #[allow(deprecated)]
            core::arch::x86_64::_mm_setcsr(prev | MXCSR_FTZ_BIT | MXCSR_DAZ_BIT);
            Self { prev_mxcsr: prev }
        }
        #[cfg(target_arch = "x86")]
        unsafe {
            #[allow(deprecated)]
            let prev = core::arch::x86::_mm_getcsr();
            #[allow(deprecated)]
            core::arch::x86::_mm_setcsr(prev | MXCSR_FTZ_BIT | MXCSR_DAZ_BIT);
            Self { prev_mxcsr: prev }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        {
            Self {}
        }
    }
}

impl Default for ScopedNoDenormals {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ScopedNoDenormals {
    #[inline]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            #[allow(deprecated)]
            core::arch::x86_64::_mm_setcsr(self.prev_mxcsr);
        }
        #[cfg(target_arch = "x86")]
        unsafe {
            #[allow(deprecated)]
            core::arch::x86::_mm_setcsr(self.prev_mxcsr);
        }
    }
}

/// Limpia manualmente valores subnormales o no finitos en `f32` por seguridad adicional en realimentaciones.
#[inline]
pub fn flush_denormal(x: f32) -> f32 {
    if !x.is_finite() || x.abs() < 1.0e-20_f32 {
        0.0
    } else {
        x
    }
}

/// Limpia manualmente valores subnormales o no finitos en `f64` dentro de integradores o lazos IIR.
#[inline]
pub fn flush_denormal_f64(x: f64) -> f64 {
    if !x.is_finite() || x.abs() < 1.0e-30_f64 {
        0.0
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_no_denormals_sets_and_restores_mxcsr() {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            #[allow(deprecated)]
            let initial = core::arch::x86_64::_mm_getcsr();
            {
                let _guard = ScopedNoDenormals::new();
                #[allow(deprecated)]
                let active = core::arch::x86_64::_mm_getcsr();
                assert_ne!(active & MXCSR_FTZ_BIT, 0);
                assert_ne!(active & MXCSR_DAZ_BIT, 0);
            }
            #[allow(deprecated)]
            let restored = core::arch::x86_64::_mm_getcsr();
            assert_eq!(restored, initial);
        }
        assert_eq!(flush_denormal(1.0e-39_f32), 0.0);
        assert_eq!(flush_denormal(0.5_f32), 0.5_f32);
    }
}
