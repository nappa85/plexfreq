//! PCM math is Rust-owned; decoders and the output sink are platform libraries.
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub crossfade_ms: u32,
    pub normalization: bool,
    pub eq: [f32; 10],
}
impl Default for Config {
    fn default() -> Self {
        Self {
            crossfade_ms: 0,
            normalization: false,
            eq: [0.; 10],
        }
    }
}
impl Config {
    pub fn validate(&self) -> crate::Result<()> {
        if self.crossfade_ms > 12000
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
}
impl Processor {
    pub fn new(rate: u32, config: &Config) -> Self {
        let frequencies: [f32; 10] = [
            31., 62., 125., 250., 500., 1000., 2000., 4000., 8000., 16000.,
        ];
        Self {
            filters: std::array::from_fn(|i| {
                Biquad::peak(
                    rate as f32,
                    frequencies[i].min(rate as f32 * 0.45),
                    config.eq[i],
                )
            }),
            eq_enabled: config.eq.iter().any(|g| *g != 0.),
        }
    }
    pub fn process(&mut self, pcm: &mut [f32], gain_db: f32, volume: f32) {
        let gain = 10f32.powf(gain_db.clamp(-30., 20.) / 20.) * volume.clamp(0., 1.);
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
/// Equal-power mixing preserves the exact overlap frame count and channel order.
pub fn crossfade(
    old: &[f32],
    new: &[f32],
    output: &mut [f32],
    start_frame: usize,
    total_frames: usize,
) {
    for (i, sample) in output.iter_mut().enumerate() {
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
