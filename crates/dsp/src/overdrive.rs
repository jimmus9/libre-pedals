//! Overdrive unificado de 6 perillas (`D6`, arquitectura **Gray-Box + `ADAA1`** validada en `D5`):
//! cubre en un único espacio continuo los caracteres TS-808 (`Pre-Cut ~720 Hz`), Boss SD-1
//! (`Asym > 50%`), Klon Centaur (`Blend` paralelo compensado `0.5` muestras) y Bluesbreaker
//! (`Pre-Cut` bajo y `Asym = 0%`).

use crate::adaa::{ln_cosh_stable, HalfSampleDelay};
use crate::oversampler::Oversampler;
use crate::smoother::OnePoleSmoother;
use crate::svf::{SvfMode, SvfTpt};
use crate::{db_to_lin, sanitize_sample, DcBlocker, Effect, ParamScale, ParamSpec};

const OVERDRIVE_PARAMS: [ParamSpec; 6] = [
    ParamSpec {
        id: "drive_db",
        name: "Drive",
        unit: "dB",
        min: 0.0,
        max: 40.0,
        default: 14.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
    ParamSpec {
        id: "low_cut_hz",
        name: "Pre-Cut",
        unit: "Hz",
        min: 30.0,
        max: 800.0,
        default: 350.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "asymmetry",
        name: "Asym",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 20.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "clean_blend",
        name: "Blend",
        unit: "%",
        min: 0.0,
        max: 100.0,
        default: 0.0,
        scale: ParamScale::Linear,
        decimals: 0,
    },
    ParamSpec {
        id: "tone_hz",
        name: "Tone",
        unit: "Hz",
        min: 500.0,
        max: 12_000.0,
        default: 4_000.0,
        scale: ParamScale::Logarithmic,
        decimals: 0,
    },
    ParamSpec {
        id: "level_db",
        name: "Level",
        unit: "dB",
        min: -36.0,
        max: 12.0,
        default: -6.0,
        scale: ParamScale::Linear,
        decimals: 1,
    },
];

/// Waveshaper híbrido acotado `tanh + tanh(0.38·v)` C1 continuo con simetría/asimetría 2:1 (SD-1) y ADAA1 exacta.
#[derive(Debug, Clone, Copy)]
struct HybridDiodeAdaa1 {
    pos_limit: f64,
    neg_limit: f64,
    prev_x: f64,
    prev_f1: f64,
}

impl HybridDiodeAdaa1 {
    const SCALE_SOFT: f64 = 0.38;
    // Margen 0.94 para mantener picos de salida <= 1.0 * level_lin incluso con rizado HIIR.
    const OUT_NORM: f64 = 0.94;

    fn new(asym_frac: f64) -> Self {
        let a = asym_frac.clamp(0.0, 1.0);
        Self {
            pos_limit: 1.0,
            neg_limit: 1.0 - 0.52 * a,
            prev_x: 0.0,
            prev_f1: 0.0,
        }
    }

    #[inline]
    fn set_asymmetry(&mut self, asym_frac: f64) {
        let a = asym_frac.clamp(0.0, 1.0);
        let next_pos = 1.0;
        let next_neg = 1.0 - 0.52 * a;
        if (next_pos - self.pos_limit).abs() > 1e-6 || (next_neg - self.neg_limit).abs() > 1e-6 {
            self.pos_limit = next_pos;
            self.neg_limit = next_neg;
            self.prev_f1 = self.antiderivative(self.prev_x);
        }
    }

    fn reset(&mut self) {
        self.prev_x = 0.0;
        self.prev_f1 = 0.0;
    }

    #[inline]
    fn shape(&self, x: f64) -> f64 {
        let lim = if x >= 0.0 {
            self.pos_limit
        } else {
            self.neg_limit
        };
        let v = x / lim;
        Self::OUT_NORM * lim * (0.85 * v.tanh() + 0.15 * (Self::SCALE_SOFT * v).tanh())
    }

    #[inline]
    fn antiderivative(&self, x: f64) -> f64 {
        let lim = if x >= 0.0 {
            self.pos_limit
        } else {
            self.neg_limit
        };
        let v = x / lim;
        Self::OUT_NORM
            * lim
            * lim
            * (0.85 * ln_cosh_stable(v)
                + (0.15 / Self::SCALE_SOFT) * ln_cosh_stable(Self::SCALE_SOFT * v))
    }

    #[inline]
    fn process_sample(&mut self, x: f64) -> f64 {
        let f1 = self.antiderivative(x);
        let dx = x - self.prev_x;
        let out = if dx.abs() < 1e-5 {
            self.shape(0.5 * (x + self.prev_x))
        } else {
            (f1 - self.prev_f1) / dx
        };
        self.prev_x = x;
        self.prev_f1 = f1;
        out
    }
}

/// Integrador TPT pasa-bajos de 1 polo para tono y suavizado anti-*fizz* sin inversión brusca de fase en 8 kHz.
#[derive(Debug, Clone, Copy)]
struct OnePoleTptLp {
    g: f64,
    state: f64,
    sample_rate: f64,
}

impl OnePoleTptLp {
    fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        let mut lp = Self {
            g: 0.0,
            state: 0.0,
            sample_rate: (sample_rate as f64).max(1.0),
        };
        lp.set_cutoff(cutoff_hz);
        lp
    }

    fn set_sample_rate(&mut self, sample_rate: f32, cutoff_hz: f32) {
        self.sample_rate = (sample_rate as f64).max(1.0);
        self.set_cutoff(cutoff_hz);
    }

    #[inline]
    fn set_cutoff(&mut self, cutoff_hz: f32) {
        let fc = (cutoff_hz as f64).clamp(20.0, 0.45 * self.sample_rate);
        self.g = (std::f64::consts::PI * fc / self.sample_rate).tan();
    }

    #[inline]
    fn reset(&mut self) {
        self.state = 0.0;
    }

    #[inline]
    fn process_sample(&mut self, x: f64) -> f64 {
        let y = (self.g * x + self.state) / (1.0 + self.g);
        self.state = 2.0 * y - self.state;
        y
    }
}

/// Pedal de overdrive unificado de 6 perillas (`Drive`, `Pre-Cut`, `Asym`, `Blend`, `Tone`, `Level`).
pub struct Overdrive {
    drive_db: f32,
    low_cut_hz: f32,
    asymmetry: f32,
    clean_blend: f32,
    tone_hz: f32,
    level_db: f32,
    sample_rate: f32,

    drive_smoother: OnePoleSmoother,
    low_cut_smoother: OnePoleSmoother,
    asym_smoother: OnePoleSmoother,
    blend_smoother: OnePoleSmoother,
    tone_smoother: OnePoleSmoother,
    level_smoother: OnePoleSmoother,

    pre_dc: DcBlocker,
    oversampler: Oversampler,
    pre_hp: SvfTpt,
    last_pre_hp_hz: f32,
    shaper: HybridDiodeAdaa1,
    fizz_lp: OnePoleTptLp,
    post_dc: DcBlocker,
    tone_lp: OnePoleTptLp,
    last_tone_hz: f32,
    clean_delay: HalfSampleDelay,
}

impl Default for Overdrive {
    fn default() -> Self {
        Self::new()
    }
}

impl Overdrive {
    pub fn new() -> Self {
        let mut s = Self {
            drive_db: 14.0,
            low_cut_hz: 350.0,
            asymmetry: 20.0,
            clean_blend: 0.0,
            tone_hz: 4_000.0,
            level_db: -6.0,
            sample_rate: 48_000.0,
            drive_smoother: OnePoleSmoother::new(8.0, 48_000.0, db_to_lin(14.0)),
            low_cut_smoother: OnePoleSmoother::new(8.0, 48_000.0, 350.0),
            asym_smoother: OnePoleSmoother::new(8.0, 48_000.0, 0.20),
            blend_smoother: OnePoleSmoother::new(8.0, 48_000.0, 0.0),
            tone_smoother: OnePoleSmoother::new(8.0, 48_000.0, 4_000.0),
            level_smoother: OnePoleSmoother::new(8.0, 48_000.0, db_to_lin(-6.0)),
            pre_dc: DcBlocker::new(15.0),
            oversampler: Oversampler::new(48_000.0, 512),
            pre_hp: SvfTpt::new(192_000.0),
            last_pre_hp_hz: -1.0,
            shaper: HybridDiodeAdaa1::new(0.20),
            fizz_lp: OnePoleTptLp::new(192_000.0, 28_000.0),
            post_dc: DcBlocker::new(18.0),
            tone_lp: OnePoleTptLp::new(192_000.0, 4_000.0),
            last_tone_hz: -1.0,
            clean_delay: HalfSampleDelay::default(),
        };
        s.prepare(48_000.0, 512);
        s
    }

    pub fn set_drive_db(&mut self, drive_db: f32) {
        let db = OVERDRIVE_PARAMS[0].clamp(drive_db);
        self.drive_db = db;
        self.drive_smoother.set_target(db_to_lin(db));
    }

    pub fn set_low_cut_hz(&mut self, hz: f32) {
        let val = OVERDRIVE_PARAMS[1].clamp(hz);
        self.low_cut_hz = val;
        self.low_cut_smoother.set_target(val);
    }

    pub fn set_asymmetry(&mut self, pct: f32) {
        let val = OVERDRIVE_PARAMS[2].clamp(pct);
        self.asymmetry = val;
        self.asym_smoother.set_target(val * 0.01);
    }

    pub fn set_clean_blend(&mut self, pct: f32) {
        let val = OVERDRIVE_PARAMS[3].clamp(pct);
        self.clean_blend = val;
        self.blend_smoother.set_target(val * 0.01);
    }

    pub fn set_tone_hz(&mut self, tone_hz: f32) {
        let val = OVERDRIVE_PARAMS[4].clamp(tone_hz);
        self.tone_hz = val;
        self.tone_smoother.set_target(val);
    }

    pub fn set_level_db(&mut self, level_db: f32) {
        let db = OVERDRIVE_PARAMS[5].clamp(level_db);
        self.level_db = db;
        self.level_smoother.set_target(db_to_lin(db));
    }

    pub fn drive_db(&self) -> f32 {
        self.drive_db
    }

    pub fn tone_hz(&self) -> f32 {
        self.tone_hz
    }

    pub fn level_db(&self) -> f32 {
        self.level_db
    }
}

impl Effect for Overdrive {
    fn kind(&self) -> &'static str {
        "overdrive"
    }

    fn name(&self) -> &'static str {
        "Overdrive"
    }

    fn accent_color(&self) -> [u8; 3] {
        [249, 115, 22]
    }

    fn params(&self) -> &'static [ParamSpec] {
        &OVERDRIVE_PARAMS
    }

    fn get_param(&self, index: usize) -> f32 {
        match index {
            0 => self.drive_db,
            1 => self.low_cut_hz,
            2 => self.asymmetry,
            3 => self.clean_blend,
            4 => self.tone_hz,
            5 => self.level_db,
            _ => 0.0,
        }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.set_drive_db(value),
            1 => self.set_low_cut_hz(value),
            2 => self.set_asymmetry(value),
            3 => self.set_clean_blend(value),
            4 => self.set_tone_hz(value),
            5 => self.set_level_db(value),
            _ => {}
        }
    }

    fn prepare(&mut self, sample_rate: f32, max_block: usize) {
        self.sample_rate = sample_rate.max(1.0);
        self.pre_dc.prepare(self.sample_rate);
        self.oversampler.prepare(self.sample_rate, max_block.max(1));
        let os_sr = self.oversampler.oversampled_sample_rate();

        self.drive_smoother.set_sample_rate(os_sr);
        self.low_cut_smoother.set_sample_rate(os_sr);
        self.asym_smoother.set_sample_rate(os_sr);
        self.blend_smoother.set_sample_rate(os_sr);
        self.tone_smoother.set_sample_rate(os_sr);
        self.level_smoother.set_sample_rate(self.sample_rate);

        self.pre_hp.set_sample_rate(os_sr);
        self.last_pre_hp_hz = -1.0;
        self.fizz_lp.set_sample_rate(os_sr, 28_000.0);
        self.post_dc.prepare(os_sr);
        self.tone_lp.set_sample_rate(os_sr, self.tone_hz);
        self.last_tone_hz = -1.0;

        self.reset();
    }

    fn process(&mut self, buf: &mut [f32]) {
        for s in buf.iter_mut() {
            *s = self.pre_dc.process_sample(sanitize_sample(*s));
        }

        let drive_sm = &mut self.drive_smoother;
        let low_cut_sm = &mut self.low_cut_smoother;
        let asym_sm = &mut self.asym_smoother;
        let blend_sm = &mut self.blend_smoother;
        let tone_sm = &mut self.tone_smoother;

        let pre_hp = &mut self.pre_hp;
        let last_pre_hp = &mut self.last_pre_hp_hz;
        let shaper = &mut self.shaper;
        let fizz_lp = &mut self.fizz_lp;
        let post_dc = &mut self.post_dc;
        let tone_lp = &mut self.tone_lp;
        let last_tone = &mut self.last_tone_hz;
        let clean_delay = &mut self.clean_delay;

        self.oversampler.process_block(buf, |os_buf, _| {
            for s in os_buf.iter_mut() {
                let x = *s;
                let drive = drive_sm.next_f64();
                let pre_hz = low_cut_sm.next_val();
                if (pre_hz - *last_pre_hp).abs() > 0.1 {
                    *last_pre_hp = pre_hz;
                    pre_hp.set_params(
                        SvfMode::HighPass,
                        pre_hz,
                        std::f32::consts::FRAC_1_SQRT_2,
                        0.0,
                    );
                }
                let asym = asym_sm.next_f64();
                shaper.set_asymmetry(asym);
                let blend = blend_sm.next_f64();
                let tone_hz = tone_sm.next_val();
                if (tone_hz - *last_tone).abs() > 0.25 {
                    *last_tone = tone_hz;
                    tone_lp.set_cutoff(tone_hz);
                }

                // Rama limpia compensada en 0.5 muestras para coincidir con el retardo ADAA1.
                let x_clean = clean_delay.process_sample(x);

                // Rama saturada TS/SD-1: Pre-Cut -> Drive -> ADAA1 -> Anti-fizz -> DC Blocker -> Tone.
                let hp_out = pre_hp.process_sample_f64(x);
                let driven = (hp_out * drive).clamp(-80.0, 80.0);
                let shaped = shaper.process_sample(driven);
                let de_fizzed = fizz_lp.process_sample(shaped);
                let no_dc = post_dc.process_sample(de_fizzed as f32) as f64;
                let toned = tone_lp.process_sample(no_dc);

                *s = (1.0 - blend) * toned + blend * x_clean;
            }
        });

        for s in buf.iter_mut() {
            let lvl = self.level_smoother.next_val();
            *s *= lvl;
        }
    }

    fn reset(&mut self) {
        self.pre_dc.reset();
        self.oversampler.reset();
        self.pre_hp.reset();
        self.fizz_lp.reset();
        self.post_dc.reset();
        self.tone_lp.reset();
        self.clean_delay.reset();
        self.shaper.reset();

        self.drive_smoother.reset(db_to_lin(self.drive_db));
        self.low_cut_smoother.reset(self.low_cut_hz);
        self.asym_smoother.reset(self.asymmetry * 0.01);
        self.blend_smoother.reset(self.clean_blend * 0.01);
        self.tone_smoother.reset(self.tone_hz);
        self.level_smoother.reset(db_to_lin(self.level_db));

        self.last_pre_hp_hz = self.low_cut_hz;
        self.pre_hp.set_params(
            SvfMode::HighPass,
            self.low_cut_hz,
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
        );
        self.last_tone_hz = self.tone_hz;
        self.tone_lp.set_cutoff(self.tone_hz);
        self.shaper.set_asymmetry((self.asymmetry * 0.01) as f64);
    }
}
