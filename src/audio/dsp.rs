//! PCM math is Rust-owned; decoders and the output sink are platform libraries.
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub crossfade_ms: u32,
    pub normalization: bool,
    pub eq: [f32; 10],
    pub normalization_mode: NormalizationMode,
    pub headroom_db: f32,
}
#[derive(Clone, Copy, Default, Deserialize, Serialize, Debug)]
#[serde(rename_all = "lowercase")]
pub enum NormalizationMode {
    #[default]
    Track,
    Album,
    Auto,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            crossfade_ms: 0,
            normalization: false,
            eq: [0.; 10],
            normalization_mode: NormalizationMode::Track,
            headroom_db: 0.,
        }
    }
}
impl Config {
    pub fn validate(&self) -> crate::Result<()> {
        if self.crossfade_ms > 12000
            || !self.headroom_db.is_finite()
            || !(0.0..=12.0).contains(&self.headroom_db)
            || self
                .eq
                .iter()
                .any(|g| !g.is_finite() || !(-12.0..=12.0).contains(g))
        {
            return Err(crate::Error::Input(
                "Audio settings exceed the supported range",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy)]
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [[f32; 2]; 2],
}
impl Biquad {
    /// Peaking EQ with fixed Q=1 (alpha = sin(w)/2). Q is intentionally not
    /// exposed: the 10-band UI only controls centre gain. Keep in sync with
    /// `Processor::new` frequencies.
    fn peak(rate: f32, freq: f32, gain: f32) -> Self {
        let a = 10f32.powf(gain / 40.);
        let w = 2. * std::f32::consts::PI * freq / rate;
        let alpha = w.sin() / 2.;
        let norm = 1. + alpha / a;
        Self {
            b: [
                (1. + alpha * a) / norm,
                (-2. * w.cos()) / norm,
                (1. - alpha * a) / norm,
            ],
            a: [(-2. * w.cos()) / norm, (1. - alpha / a) / norm],
            z: [[0.; 2]; 2],
        }
    }
    fn sample(&mut self, input: f32, channel: usize) -> f32 {
        let z = &mut self.z[channel];
        let out = self.b[0] * input + z[0];
        z[0] = self.b[1] * input - self.a[0] * out + z[1];
        z[1] = self.b[2] * input - self.a[1] * out;
        out
    }
}
pub struct Processor {
    filters: [Biquad; 10],
    eq_enabled: bool,
    headroom: f32,
}
impl Processor {
    pub fn new(rate: u32, config: &Config) -> Self {
        // RATE is fixed at 48kHz; clamp degenerate rates instead of dividing
        // by zero and poisoning every filter with NaNs.
        let rate = rate.max(8000);
        let frequencies: [f32; 10] = [
            31., 62., 125., 250., 500., 1000., 2000., 4000., 8000., 16000.,
        ];
        // Never trust persisted config: NaN/out-of-range headroom or EQ would
        // poison every sample (NaN.clamp returns NaN) and be pushed to appsrc.
        // Mirror Config::validate here so direct construction is safe.
        let headroom_db =
            if config.headroom_db.is_finite() && (0.0..=12.0).contains(&config.headroom_db) {
                config.headroom_db
            } else {
                0.0
            };
        let eq: [f32; 10] = std::array::from_fn(|i| {
            let g = config.eq[i];
            if g.is_finite() {
                g.clamp(-12.0, 12.0)
            } else {
                0.0
            }
        });
        Self {
            filters: std::array::from_fn(|i| {
                Biquad::peak(rate as f32, frequencies[i].min(rate as f32 * 0.45), eq[i])
            }),
            eq_enabled: eq.iter().any(|g| *g != 0.),
            headroom: 10f32.powf(-headroom_db / 20.),
        }
    }
    pub fn process(&mut self, pcm: &mut [f32], gain_db: f32, volume: f32) {
        let gain = amplitude(gain_db) * volume.clamp(0., 1.) * self.headroom;
        for (i, sample) in pcm.iter_mut().enumerate() {
            let mut value = if sample.is_finite() { *sample } else { 0. };
            if self.eq_enabled {
                for filter in &mut self.filters {
                    value = filter.sample(value, i % 2);
                }
            }
            *sample = (value * gain).clamp(-1., 1.);
        }
    }
}
pub fn amplitude(gain_db: f32) -> f32 {
    let db = if gain_db.is_finite() {
        gain_db.clamp(-30., 20.)
    } else {
        0.
    };
    10f32.powf(db / 20.)
}
/// Equal-power mixing preserves the exact overlap frame count and channel order.
/// Buffers are stereo-interleaved pairs; only the shortest shared prefix is
/// mixed so a short tail can never panic the audio thread. Callers pass
/// equal-length blocks in the steady state.
pub fn crossfade(
    old: &[f32],
    new: &[f32],
    output: &mut [f32],
    start_frame: usize,
    total_frames: usize,
) {
    let len = old.len().min(new.len()).min(output.len());
    // Stereo frames: two samples per progress step.
    for (i, sample) in output.iter_mut().enumerate().take(len) {
        let progress = ((start_frame + i / 2) as f32 / total_frames.max(1) as f32).clamp(0., 1.);
        let angle = progress * std::f32::consts::FRAC_PI_2;
        *sample = old[i] * angle.cos() + new[i] * angle.sin();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gain_limiter_and_nan_handling() {
        let mut p = Processor::new(48000, &Config::default());
        let mut pcm = [0.5, -0.5, 2., f32::NAN];
        p.process(&mut pcm, 0., 0.5);
        assert_eq!(pcm, [0.25, -0.25, 1., 0.]);
    }
    #[test]
    fn headroom_preserves_unclipped_crossfade_peaks_and_bounds_bad_gain() {
        let config = Config {
            headroom_db: 6.,
            ..Default::default()
        };
        let mut processor = Processor::new(48000, &config);
        let mut pcm = [std::f32::consts::SQRT_2; 2];
        processor.process(&mut pcm, 0., 1.);
        assert!((pcm[0] - 0.7087858).abs() < 1e-5);
        assert_eq!(amplitude(f32::INFINITY), 1.);
        assert_eq!(amplitude(f32::NAN), 1.);
        assert_eq!(amplitude(1000.), 10.);
    }
    #[test]
    fn equal_power_fade_has_expected_endpoints_and_midpoint() {
        let old = [1.; 4];
        let new = [0.; 4];
        let mut out = [0.; 4];
        crossfade(&old, &new, &mut out, 0, 2);
        assert_eq!(out[0], 1.);
        assert!((out[2] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        crossfade(&old, &new, &mut out, 2, 2);
        assert!(out[0].abs() < 1e-6);
    }
    #[test]
    fn eq_changes_target_band_and_is_stable() {
        let mut config = Config::default();
        config.eq[5] = 6.;
        let mut p = Processor::new(48000, &config);
        let mut pcm = Vec::new();
        for n in 0..48000 {
            let x = 0.05 * (2. * std::f32::consts::PI * 1000. * n as f32 / 48000.).sin();
            pcm.extend([x, x]);
        }
        p.process(&mut pcm, 0., 1.);
        let peak = pcm[48000..].iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!(peak > 0.09 && peak < 0.11);
    }
}
