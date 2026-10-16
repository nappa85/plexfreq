//! The continuous PCM output of one load/seek. Frames are counted from zero,
//! the same domain as the actor's gapless segment coordinates. PulseAudio is
//! production; the fake (real-time, silent) and capture (instant, recorded)
//! sinks keep tests deterministic without touching system audio.
use super::{lock_capture, pulse, Sink, CHANNELS, RATE};
use crate::{Error, Result};
use std::{
    rc::Rc,
    sync::{Arc, Mutex},
    time::Instant,
};

/// Captured PCM is bounded (120 seconds of stereo) in long test sessions.
const CAPTURE_LIMIT: usize = RATE as usize * 120 * CHANNELS;

#[derive(Default)]
pub(super) struct PositionClock {
    pub raw: u64,
    pub offset: u64,
    position: u64,
    pub available: bool,
}
impl PositionClock {
    pub fn observe(&mut self, raw: Option<u64>, submitted: u64) -> u64 {
        self.available = raw.is_some();
        if let Some(raw) = raw {
            // A route change or server restart of the timing source must not
            // move the gapless frame domain backwards. Small backwards query
            // jitter is clamped, not accumulated as drift.
            if raw.saturating_add(u64::from(RATE)) <= self.raw {
                self.offset = self.position.saturating_sub(raw);
                crate::diagnostics::event(format_args!(
                    "output position clock reset previous_frames={} raw_frames={raw} offset_frames={}",
                    self.raw, self.offset
                ));
            }
            self.raw = raw;
            self.position = self
                .position
                .max(raw.saturating_add(self.offset))
                .min(submitted);
        }
        self.position
    }
}

enum Backend {
    Pulse {
        stream: pulse::Stream,
        events: pulse::Events,
    },
    /// Consumes queued frames in real time while uncorked.
    Fake { consumed: u64, since: Instant },
    /// Consumes instantly while uncorked, recording the PCM.
    Capture {
        capture: Arc<Mutex<Vec<f32>>>,
        pending: Vec<f32>,
        consumed: u64,
    },
}

pub(super) struct Output {
    /// Frames submitted since this output was created.
    pub frames: u64,
    /// All submitted frames were played after `finish`.
    pub eof: bool,
    pub warnings: u64,
    pub position_clock: PositionClock,
    corked: bool,
    finishing: bool,
    backend: Backend,
    /// Test hook: policy requests injected into simulated sinks.
    #[cfg(test)]
    pub injected: pulse::Events,
}

impl Output {
    pub fn new(
        mode: Sink,
        capture: Arc<Mutex<Vec<f32>>>,
        connection: &mut Option<Rc<pulse::Connection>>,
    ) -> Result<Self> {
        let backend = match mode {
            Sink::Pulse => {
                if connection.as_ref().is_none_or(|c| !c.ready()) {
                    *connection = None;
                    *connection = Some(pulse::Connection::new().map_err(|reason| {
                        eprintln!("PlexFreq audio output unavailable: {reason}");
                        Error::Input("Audio output is unavailable")
                    })?);
                }
                let connection = connection
                    .clone()
                    .ok_or(Error::Input("Audio output is unavailable"))?;
                let stream = pulse::Stream::new(
                    connection,
                    &std::fs::read_to_string("/etc/os-release").unwrap_or_default(),
                )
                .map_err(|reason| {
                    eprintln!("PlexFreq audio output unavailable: {reason}");
                    Error::Input("Audio output is unavailable")
                })?;
                Backend::Pulse {
                    stream,
                    events: pulse::Events::default(),
                }
            }
            Sink::Fake => Backend::Fake {
                consumed: 0,
                since: Instant::now(),
            },
            Sink::Capture => Backend::Capture {
                capture,
                pending: Vec::new(),
                consumed: 0,
            },
        };
        Ok(Self {
            frames: 0,
            eof: false,
            warnings: 0,
            position_clock: PositionClock::default(),
            corked: true,
            finishing: false,
            backend,
            #[cfg(test)]
            injected: pulse::Events::default(),
        })
    }
    /// Process sink notifications. A policy cork request (call, alarm,
    /// higher-priority stream) pauses and clears play intent; an uncork
    /// request is not permission to resume a user/disconnect pause.
    pub fn service(&mut self, desired: &mut bool) -> Result<()> {
        #[cfg(test)]
        let injected = std::mem::take(&mut self.injected);
        #[cfg(not(test))]
        let injected = pulse::Events::default();
        let mut cork_requests = injected.cork_requests;
        let mut uncork_requests = injected.uncork_requests;
        if injected.failed {
            eprintln!("PlexFreq audio output error: simulated failure");
            return Err(Error::Input("Audio output is unavailable"));
        }
        if let Backend::Pulse { stream, events } = &mut self.backend {
            let facts = stream.service().map_err(|reason| {
                eprintln!("PlexFreq audio output error: {reason}");
                Error::Input("Audio output is unavailable")
            })?;
            if facts.underflows != events.underflows {
                self.warnings += facts.underflows - events.underflows;
                crate::diagnostics::event(format_args!(
                    "output underflow count={} queued_frames={}",
                    facts.underflows,
                    self.frames.saturating_sub(self.position_clock.position)
                ));
            }
            cork_requests += facts.cork_requests;
            uncork_requests += facts.uncork_requests;
            if facts.drained && self.finishing {
                self.eof = true;
            }
            *events = facts;
        }
        if uncork_requests > 0 {
            crate::diagnostics::event(format_args!("output policy requested uncork (ignored)"));
        }
        if cork_requests > 0 {
            crate::diagnostics::event(format_args!("output policy requested cork"));
            *desired = false;
            self.set_playing(false)?;
        }
        if self.finishing && !matches!(self.backend, Backend::Pulse { .. }) {
            self.eof = self.consumed() >= self.frames;
        }
        Ok(())
    }
    pub fn push(&mut self, pcm: &[f32]) -> Result<()> {
        let frames = (pcm.len() / CHANNELS) as u64;
        let corked = self.corked;
        match &mut self.backend {
            Backend::Pulse { stream, .. } => stream
                .write(pcm)
                .map_err(|_| Error::Input("Audio output failed"))?,
            Backend::Fake { .. } => {}
            Backend::Capture {
                capture,
                pending,
                consumed,
            } => {
                if corked {
                    pending.extend_from_slice(pcm);
                } else {
                    let mut captured = lock_capture(capture);
                    if captured.len() < CAPTURE_LIMIT {
                        captured.extend_from_slice(pcm);
                    }
                    *consumed += frames;
                }
            }
        }
        self.frames += frames;
        Ok(())
    }
    /// Uncork (play) or cork (pause) the stream; queued audio is retained.
    pub fn set_playing(&mut self, playing: bool) -> Result<()> {
        if self.corked != playing {
            return Ok(());
        }
        // Account fake playback up to the transition.
        self.consumed();
        self.corked = !playing;
        crate::diagnostics::event(format_args!("output corked={}", self.corked));
        match &mut self.backend {
            Backend::Pulse { stream, .. } => stream
                .cork(!playing)
                .map_err(|_| Error::Input("Audio output is unavailable"))?,
            Backend::Fake { since, .. } => *since = Instant::now(),
            Backend::Capture {
                capture,
                pending,
                consumed,
            } => {
                if playing && !pending.is_empty() {
                    let mut captured = lock_capture(capture);
                    if captured.len() < CAPTURE_LIMIT {
                        captured.extend_from_slice(pending);
                    }
                    *consumed += (pending.len() / CHANNELS) as u64;
                    pending.clear();
                }
            }
        }
        Ok(())
    }
    /// No more PCM follows: play out the queue, then report `eof`.
    pub fn finish(&mut self) -> Result<()> {
        self.finishing = true;
        if let Backend::Pulse { stream, .. } = &mut self.backend {
            stream
                .drain()
                .map_err(|_| Error::Input("Audio output is unavailable"))?;
        }
        Ok(())
    }
    /// Frames consumed by a simulated sink (submitted for PulseAudio).
    fn consumed(&mut self) -> u64 {
        let corked = self.corked;
        let frames = self.frames;
        match &mut self.backend {
            Backend::Pulse { .. } => frames,
            Backend::Fake { consumed, since } => {
                if !corked {
                    let now = Instant::now();
                    let elapsed = now.duration_since(*since);
                    let advance = (elapsed.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64;
                    let advance = advance.min(frames - *consumed);
                    if advance > 0 {
                        *consumed += advance;
                        // Keep the sub-frame remainder of elapsed time.
                        *since += std::time::Duration::from_nanos(
                            (u128::from(advance) * 1_000_000_000 / u128::from(RATE)) as u64,
                        );
                    }
                    if *consumed == frames {
                        *since = now;
                    }
                }
                *consumed
            }
            Backend::Capture { consumed, .. } => *consumed,
        }
    }
    /// Audible position in output frames.
    pub fn position(&mut self) -> u64 {
        let raw = match &mut self.backend {
            Backend::Pulse { stream, .. } => stream.played(),
            _ => Some(self.consumed()),
        };
        self.position_clock.observe(raw, self.frames)
    }
    /// Queued (submitted but not yet audible) PCM in bytes.
    pub fn queued_bytes(&mut self) -> u64 {
        let position = self.position();
        self.frames.saturating_sub(position) * (CHANNELS * 4) as u64
    }
    /// Uncorked and actually rendering (not prebuffering or underrun).
    pub fn playing(&self) -> bool {
        !self.corked
            && match &self.backend {
                Backend::Pulse { events, .. } => events.running,
                _ => true,
            }
    }
    pub fn corked(&self) -> bool {
        self.corked
    }
}
