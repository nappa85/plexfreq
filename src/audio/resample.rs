//! Stereo f32 conversion to the engine rate. Rubato's asynchronous sinc
//! resampler is used with a fixed ratio: pure Rust, no FFT dependency, and a
//! filter comparable to GStreamer audioresample's default quality.
use rubato::{
    audioadapter_buffers::direct::InterleavedSlice, Async, FixedAsync, Indexing, Resampler as _,
    SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

const CHANNELS: usize = 2;
const CHUNK: usize = 1024;
const SINC_LEN: usize = 128;

pub(super) struct Resampler {
    inner: Async<f32>,
    ratio: f64,
    /// Interleaved stereo input not yet processed.
    pending: Vec<f32>,
    output: Vec<f32>,
    /// Output frames still to discard (filter delay plus edge pre-roll).
    discard: u64,
    input_frames: u64,
    output_frames: u64,
    primed: bool,
    last: [f32; CHANNELS],
}
impl Resampler {
    pub fn new(from: u32, to: u32) -> Option<Self> {
        let ratio = f64::from(to) / f64::from(from);
        let parameters = SincInterpolationParameters {
            sinc_len: SINC_LEN,
            f_cutoff: None,
            oversampling_factor: 256,
            interpolation: SincInterpolationType::Linear,
            window: WindowFunction::BlackmanHarris2,
        };
        let inner =
            Async::<f32>::new_sinc(ratio, 1.0, &parameters, CHUNK, CHANNELS, FixedAsync::Input)
                .ok()?;
        let output = vec![0.; inner.output_frames_max() * CHANNELS];
        Some(Self {
            discard: inner.output_delay() as u64,
            inner,
            ratio,
            pending: Vec::with_capacity(CHUNK * CHANNELS * 2),
            output,
            input_frames: 0,
            output_frames: 0,
            primed: false,
            last: [0.; CHANNELS],
        })
    }
    /// Resample interleaved stereo `input`, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if input.len() < CHANNELS {
            return;
        }
        if !self.primed {
            // Extend the first frame backwards instead of starting from
            // silence, so a track that begins at a non-zero level has no
            // filter ramp. The pre-roll's output is discarded below.
            self.primed = true;
            let pre = SINC_LEN / 2;
            for _ in 0..pre {
                self.pending.extend_from_slice(&input[..CHANNELS]);
            }
            self.discard += (pre as f64 * self.ratio).round() as u64;
        }
        self.last.copy_from_slice(&input[input.len() - CHANNELS..]);
        self.input_frames += (input.len() / CHANNELS) as u64;
        self.pending.extend_from_slice(input);
        while self.pending.len() >= CHUNK * CHANNELS {
            self.run(None, out);
        }
    }
    /// Emit the remaining output so the total equals input × ratio (rounded).
    pub fn finish(&mut self, out: &mut Vec<f32>) {
        if !self.primed {
            return;
        }
        let expected = (self.input_frames as f64 * self.ratio).round() as u64;
        // Hold the final frame (rather than silence) while the filter drains.
        let mut guard = 0;
        while self.output_frames < expected && guard < 64 {
            guard += 1;
            while self.pending.len() < CHUNK * CHANNELS {
                self.pending.extend_from_slice(&self.last);
            }
            self.run(Some(expected), out);
        }
    }
    fn run(&mut self, limit: Option<u64>, out: &mut Vec<f32>) {
        let frames = self.inner.output_frames_next();
        let input = InterleavedSlice::new(&self.pending[..CHUNK * CHANNELS], CHANNELS, CHUNK);
        let capacity = self.output.len() / CHANNELS;
        let output = InterleavedSlice::new_mut(&mut self.output, CHANNELS, capacity);
        let (Ok(input), Ok(mut output)) = (input, output) else {
            self.pending.clear();
            return;
        };
        let written = match self
            .inner
            .process_into_buffer(&input, &mut output, None::<&Indexing>)
        {
            Ok((_, written)) => written.min(frames),
            Err(_) => 0,
        };
        self.pending.drain(..CHUNK * CHANNELS);
        let skip = self.discard.min(written as u64) as usize;
        self.discard -= skip as u64;
        let mut keep = written - skip;
        if let Some(limit) = limit {
            keep = keep.min(limit.saturating_sub(self.output_frames) as usize);
        }
        out.extend_from_slice(&self.output[skip * CHANNELS..(skip + keep) * CHANNELS]);
        self.output_frames += keep as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_rate_conversion_has_exact_length_and_no_edge_ramp() {
        for (from, frames) in [
            (44100u32, 4410usize),
            (96000, 9600),
            (22050, 2205),
            (44100, 3),
        ] {
            let mut resampler = Resampler::new(from, 48000).unwrap();
            let input = vec![0.125f32; frames * CHANNELS];
            let mut out = Vec::new();
            // Uneven chunks, as decoded packets arrive.
            for chunk in input.chunks(7 * CHANNELS) {
                resampler.process(chunk, &mut out);
            }
            resampler.finish(&mut out);
            let expected = (frames as f64 * 48000. / f64::from(from)).round() as usize;
            assert_eq!(out.len(), expected * CHANNELS, "{from}");
            assert!(
                out.iter().all(|x| (*x - 0.125).abs() < 1e-3),
                "{from}: {:?}",
                out.iter().fold(0f32, |m, x| m.max((x - 0.125).abs()))
            );
        }
    }
}
