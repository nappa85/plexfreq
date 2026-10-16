//! PulseAudio playback through libpulse's asynchronous API. The mainloop is
//! not threaded: the audio actor dispatches it without blocking on every
//! iteration, so all callbacks run on the actor thread and need no locking.
use libpulse_sys as pa;
use std::{
    cell::RefCell,
    ffi::{c_void, CStr, CString},
    ptr,
    rc::Rc,
    time::{Duration, Instant},
};

/// Connection/stream setup budget; failure reports an unavailable output.
const SETUP: Duration = Duration::from_secs(2);
/// Server-side target buffer and request size. These match the previous
/// pulsesink buffer-time/latency-time: tolerant of competing UI work without
/// asking the shared audio server for low-latency playback.
const TARGET_MS: u32 = 300;
const REQUEST_MS: u32 = 20;
const FRAME_BYTES: u32 = 8;

fn bytes(milliseconds: u32) -> u32 {
    super::RATE / 1000 * milliseconds * FRAME_BYTES
}

/// Microseconds of stream time to frames, without overflow on long sessions.
pub(super) fn usec_to_frames(usec: u64) -> u64 {
    (u128::from(usec) * u128::from(super::RATE) / 1_000_000).min(u128::from(u64::MAX)) as u64
}

/// Stream properties. Sailfish mainvolume/stream-restore recognizes x-maemo,
/// not the generic desktop music role; the user-session policy then owns
/// volume keys and routing. Desktop systems get the standard music role.
pub(super) fn properties(os_release: &str) -> [(&'static str, &'static str); 4] {
    let sailfish = os_release
        .lines()
        .any(|line| line == "ID=sailfishos" || line == "ID=\"sailfishos\"");
    [
        ("media.role", if sailfish { "x-maemo" } else { "music" }),
        ("media.name", "PlexFreq"),
        ("application.name", "PlexFreq"),
        ("application.id", "harbour-plexfreq"),
    ]
}

/// Owned proplist, freed on every path.
struct Proplist(*mut pa::pa_proplist);
impl Proplist {
    fn new(entries: &[(&str, &str)]) -> Option<Self> {
        // SAFETY: pa_proplist_new has no preconditions; ownership is ours.
        let list = Self(unsafe { pa::pa_proplist_new() });
        if list.0.is_null() {
            return None;
        }
        for (key, value) in entries {
            let (key, value) = (CString::new(*key).ok()?, CString::new(*value).ok()?);
            // SAFETY: live proplist and NUL-terminated strings; libpulse copies them.
            unsafe { pa::pa_proplist_sets(list.0, key.as_ptr(), value.as_ptr()) };
        }
        Some(list)
    }
}
impl Drop for Proplist {
    fn drop(&mut self) {
        // SAFETY: exclusively owned, non-null proplist.
        unsafe { pa::pa_proplist_free(self.0) }
    }
}

/// One client connection per engine, reused by successive streams.
pub(super) struct Connection {
    mainloop: *mut pa::pa_mainloop,
    context: *mut pa::pa_context,
}
impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: owned pointers (or null); streams hold an Rc and are
        // therefore already gone. Context is released before its mainloop.
        unsafe {
            if !self.context.is_null() {
                pa::pa_context_disconnect(self.context);
                pa::pa_context_unref(self.context);
            }
            if !self.mainloop.is_null() {
                pa::pa_mainloop_free(self.mainloop);
            }
        }
    }
}
impl Connection {
    pub fn new() -> Result<Rc<Self>, &'static str> {
        let mut connection = Self {
            mainloop: ptr::null_mut(),
            context: ptr::null_mut(),
        };
        let properties = Proplist::new(&[
            ("application.name", "PlexFreq"),
            ("application.id", "harbour-plexfreq"),
        ])
        .ok_or("proplist allocation")?;
        // SAFETY: objects are created, owned and used only on this thread.
        unsafe {
            connection.mainloop = pa::pa_mainloop_new();
            if connection.mainloop.is_null() {
                return Err("mainloop allocation");
            }
            connection.context = pa::pa_context_new_with_proplist(
                pa::pa_mainloop_get_api(connection.mainloop),
                c"PlexFreq".as_ptr(),
                properties.0,
            );
            if connection.context.is_null() {
                return Err("context allocation");
            }
            // Default server (PULSE_SERVER/client.conf, as pulsesink used);
            // never spawn a daemon.
            if pa::pa_context_connect(
                connection.context,
                ptr::null(),
                pa::PA_CONTEXT_NOAUTOSPAWN,
                ptr::null(),
            ) < 0
            {
                return Err("connect failed");
            }
        }
        let deadline = Instant::now() + SETUP;
        loop {
            match connection.state() {
                pa::PA_CONTEXT_READY => return Ok(Rc::new(connection)),
                pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED => {
                    return Err("connection denied/failed")
                }
                _ if Instant::now() >= deadline => return Err("connection timeout"),
                _ => connection.wait(Duration::from_millis(20))?,
            }
        }
    }
    fn state(&self) -> pa::pa_context_state_t {
        // SAFETY: live context confined to this thread.
        unsafe { pa::pa_context_get_state(self.context) }
    }
    pub fn ready(&self) -> bool {
        self.state() == pa::PA_CONTEXT_READY
    }
    /// Dispatch pending events, sleeping at most `timeout` for new ones.
    fn wait(&self, timeout: Duration) -> Result<(), &'static str> {
        let timeout = timeout.as_micros().min(i32::MAX as u128) as i32;
        // SAFETY: live, thread-confined loop; pa_mainloop_iterate in steps.
        let failed = unsafe {
            pa::pa_mainloop_prepare(self.mainloop, timeout) < 0
                || pa::pa_mainloop_poll(self.mainloop) < 0
                || pa::pa_mainloop_dispatch(self.mainloop) < 0
        };
        if failed {
            Err("mainloop failed")
        } else {
            Ok(())
        }
    }
    /// Run callbacks for everything already received; never blocks.
    pub fn dispatch(&self) -> Result<(), &'static str> {
        self.wait(Duration::ZERO)
    }
}

/// Facts reported by stream callbacks since the last service call.
#[derive(Default, Debug, Clone, Copy)]
pub(super) struct Events {
    pub failed: bool,
    pub cork_requests: u32,
    pub uncork_requests: u32,
    /// Playback started (after prebuffering) and has not underrun since.
    pub running: bool,
    pub underflows: u64,
    pub drained: bool,
}

extern "C" fn state_callback(stream: *mut pa::pa_stream, userdata: *mut c_void) {
    // SAFETY: userdata is the stream's boxed RefCell, alive until callbacks
    // are cleared in Stream::drop; the stream pointer is live during dispatch.
    let (events, state) = unsafe {
        (
            &*userdata.cast::<RefCell<Events>>(),
            pa::pa_stream_get_state(stream),
        )
    };
    if matches!(state, pa::PA_STREAM_FAILED | pa::PA_STREAM_TERMINATED) {
        events.borrow_mut().failed = true;
    }
}
extern "C" fn event_callback(
    _: *mut pa::pa_stream,
    name: *const std::ffi::c_char,
    _: *mut pa::pa_proplist,
    userdata: *mut c_void,
) {
    if name.is_null() {
        return;
    }
    // SAFETY: as state_callback; name is a NUL-terminated libpulse string.
    let (events, name) = unsafe { (&*userdata.cast::<RefCell<Events>>(), CStr::from_ptr(name)) };
    let name = name.to_bytes();
    let mut events = events.borrow_mut();
    if name == pa::PA_STREAM_EVENT_REQUEST_CORK.as_bytes() {
        events.cork_requests += 1;
    } else if name == pa::PA_STREAM_EVENT_REQUEST_UNCORK.as_bytes() {
        events.uncork_requests += 1;
    }
}
extern "C" fn started_callback(_: *mut pa::pa_stream, userdata: *mut c_void) {
    // SAFETY: as state_callback.
    unsafe { &*userdata.cast::<RefCell<Events>>() }
        .borrow_mut()
        .running = true;
}
extern "C" fn underflow_callback(_: *mut pa::pa_stream, userdata: *mut c_void) {
    // SAFETY: as state_callback.
    let mut events = unsafe { &*userdata.cast::<RefCell<Events>>() }.borrow_mut();
    events.running = false;
    events.underflows += 1;
}
extern "C" fn drain_callback(_: *mut pa::pa_stream, success: i32, userdata: *mut c_void) {
    if success != 0 {
        // SAFETY: as state_callback; the drain operation is cancelled before
        // the userdata is freed.
        unsafe { &*userdata.cast::<RefCell<Events>>() }
            .borrow_mut()
            .drained = true;
    }
}

/// One playback stream (float32 stereo at the engine rate). Created per
/// load/seek, so its stream time starts at zero with the output's frames.
pub(super) struct Stream {
    stream: *mut pa::pa_stream,
    drain: *mut pa::pa_operation,
    events: Box<RefCell<Events>>,
    connection: Rc<Connection>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: owned stream/operation; callbacks are cleared and the drain
        // operation cancelled before the userdata box is freed. Disconnect
        // removes the sink input immediately, discarding queued audio.
        unsafe {
            if !self.drain.is_null() {
                pa::pa_operation_cancel(self.drain);
                pa::pa_operation_unref(self.drain);
            }
            pa::pa_stream_set_state_callback(self.stream, None, ptr::null_mut());
            pa::pa_stream_set_event_callback(self.stream, None, ptr::null_mut());
            pa::pa_stream_set_started_callback(self.stream, None, ptr::null_mut());
            pa::pa_stream_set_underflow_callback(self.stream, None, ptr::null_mut());
            pa::pa_stream_disconnect(self.stream);
            pa::pa_stream_unref(self.stream);
        }
    }
}
impl Stream {
    pub fn new(connection: Rc<Connection>, os_release: &str) -> Result<Self, &'static str> {
        let properties = Proplist::new(&properties(os_release)).ok_or("proplist allocation")?;
        let spec = pa::pa_sample_spec {
            format: pa::PA_SAMPLE_FLOAT32LE,
            rate: super::RATE,
            channels: 2,
        };
        let mut map = pa::pa_channel_map {
            channels: 0,
            map: [pa::PA_CHANNEL_POSITION_INVALID; pa::PA_CHANNELS_MAX as usize],
        };
        // SAFETY: valid out-pointers; the stream is owned by the returned
        // value (or released by its Drop on error paths below).
        let stream = unsafe {
            pa::pa_channel_map_init_stereo(&mut map);
            pa::pa_stream_new_with_proplist(
                connection.context,
                c"PlexFreq".as_ptr(),
                &spec,
                &map,
                properties.0,
            )
        };
        if stream.is_null() {
            return Err("stream allocation");
        }
        let output = Self {
            stream,
            drain: ptr::null_mut(),
            events: Box::new(RefCell::new(Events::default())),
            connection,
        };
        let userdata = ptr::from_ref(&*output.events).cast_mut().cast::<c_void>();
        let attributes = pa::pa_buffer_attr {
            maxlength: u32::MAX,
            tlength: bytes(TARGET_MS),
            prebuf: u32::MAX,
            minreq: bytes(REQUEST_MS),
            fragsize: u32::MAX,
        };
        // SAFETY: live stream; userdata outlives the callbacks (see Drop).
        let connected = unsafe {
            pa::pa_stream_set_state_callback(stream, Some(state_callback), userdata);
            pa::pa_stream_set_event_callback(stream, Some(event_callback), userdata);
            pa::pa_stream_set_started_callback(stream, Some(started_callback), userdata);
            pa::pa_stream_set_underflow_callback(stream, Some(underflow_callback), userdata);
            pa::pa_stream_connect_playback(
                stream,
                ptr::null(),
                &attributes,
                pa::PA_STREAM_START_CORKED
                    | pa::PA_STREAM_INTERPOLATE_TIMING
                    | pa::PA_STREAM_AUTO_TIMING_UPDATE
                    | pa::PA_STREAM_ADJUST_LATENCY,
                ptr::null(),
                ptr::null_mut(),
            )
        };
        if connected < 0 {
            return Err("stream connect failed");
        }
        let deadline = Instant::now() + SETUP;
        loop {
            // SAFETY: live stream confined to this thread.
            match unsafe { pa::pa_stream_get_state(stream) } {
                pa::PA_STREAM_READY => return Ok(output),
                pa::PA_STREAM_FAILED | pa::PA_STREAM_TERMINATED => {
                    return Err("stream denied/failed")
                }
                _ if Instant::now() >= deadline => return Err("stream setup timeout"),
                _ => output.connection.wait(Duration::from_millis(20))?,
            }
        }
    }
    /// Dispatch callbacks and return the facts they reported; request
    /// counters are consumed by this call.
    pub fn service(&mut self) -> Result<Events, &'static str> {
        self.connection.dispatch()?;
        let mut events = self.events.borrow_mut();
        if events.failed || !self.connection.ready() {
            return Err("stream failed");
        }
        let facts = Events {
            failed: false,
            cork_requests: std::mem::take(&mut events.cork_requests),
            uncork_requests: std::mem::take(&mut events.uncork_requests),
            running: events.running,
            underflows: events.underflows,
            drained: events.drained,
        };
        Ok(facts)
    }
    pub fn write(&mut self, pcm: &[f32]) -> Result<(), &'static str> {
        // SAFETY: live stream; without a free callback libpulse copies the
        // data before returning. Writes beyond the target are queued by the
        // server (maxlength is the server maximum); the actor bounds them.
        let result = unsafe {
            pa::pa_stream_write(
                self.stream,
                pcm.as_ptr().cast(),
                std::mem::size_of_val(pcm),
                None,
                0,
                pa::PA_SEEK_RELATIVE,
            )
        };
        if result < 0 {
            Err("stream write failed")
        } else {
            Ok(())
        }
    }
    pub fn cork(&mut self, corked: bool) -> Result<(), &'static str> {
        // SAFETY: live stream; the operation has no callback/userdata and our
        // reference is released at once.
        let operation =
            unsafe { pa::pa_stream_cork(self.stream, i32::from(corked), None, ptr::null_mut()) };
        if operation.is_null() {
            return Err("stream cork failed");
        }
        // SAFETY: owned operation reference.
        unsafe { pa::pa_operation_unref(operation) };
        Ok(())
    }
    /// Play out everything queued (even below prebuf); `drained` follows.
    pub fn drain(&mut self) -> Result<(), &'static str> {
        if !self.drain.is_null() {
            return Ok(());
        }
        let userdata = ptr::from_ref(&*self.events).cast_mut().cast::<c_void>();
        // SAFETY: live stream; the operation is kept and cancelled in Drop.
        self.drain = unsafe { pa::pa_stream_drain(self.stream, Some(drain_callback), userdata) };
        if self.drain.is_null() {
            Err("stream drain failed")
        } else {
            Ok(())
        }
    }
    /// Audible stream time in frames: data read by the server minus the
    /// sink latency, interpolated between timing updates. None before the
    /// first timing update.
    pub fn played(&mut self) -> Option<u64> {
        let mut usec: pa::pa_usec_t = 0;
        // SAFETY: live stream and valid out-pointer.
        (unsafe { pa::pa_stream_get_time(self.stream, &mut usec) } == 0)
            .then(|| usec_to_frames(usec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_volume_policy_roles_are_platform_specific() {
        let role = |os: &str| {
            properties(os)
                .iter()
                .find(|(key, _)| *key == "media.role")
                .unwrap()
                .1
        };
        assert_eq!(role("ID=sailfishos\n"), "x-maemo");
        assert_eq!(role("NAME=x\nID=\"sailfishos\"\n"), "x-maemo");
        assert_eq!(role("ID=gentoo\n"), "music");
        assert!(properties("").contains(&("application.name", "PlexFreq")));
    }

    #[test]
    fn stream_time_conversion_does_not_overflow_on_long_sessions() {
        assert_eq!(usec_to_frames(1_000_000), u64::from(super::super::RATE));
        // ~5 days of continuous output (the old u64 PTS overflow horizon is
        // ~106 hours) and the extreme value stay exact or saturate.
        let days = 5 * 24 * 3600 * 1_000_000u64;
        assert_eq!(
            usec_to_frames(days),
            5 * 24 * 3600 * u64::from(super::super::RATE)
        );
        assert_eq!(
            usec_to_frames(u64::MAX),
            (u128::from(u64::MAX) * 48000 / 1_000_000) as u64
        );
        assert_eq!(bytes(TARGET_MS), 48 * 300 * 8);
    }
}
