//! Read-only libpulse observation. Native objects and callbacks stay on their
//! owning worker; no subprocess, routing write, audio stream or daemon spawn.
use libpulse_sys as pa;
use std::{
    cell::RefCell,
    ffi::{c_void, CStr, CString},
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(2);
const MAX_ENTRIES: usize = 32;
#[path = "pulse/recording.rs"]
mod recording;
pub(super) use recording::watch_recording;

#[derive(Default, Debug)]
pub(super) struct Snapshot {
    inputs: usize,
    streams: Vec<String>,
    sinks: Vec<String>,
    inputs_done: bool,
    sinks_done: bool,
    error: Option<&'static str>,
}
impl Snapshot {
    pub fn streams(&self) -> String {
        format!(
            "backend=libpulse inputs={} plexfreq_streams={} [{}]",
            self.inputs,
            self.streams.len(),
            self.streams.join(" | ")
        )
    }
    pub fn routes(&self) -> String {
        format!(
            "backend=libpulse sinks={} [{}]",
            self.sinks.len(),
            self.sinks.join(" | ")
        )
    }
}

// Own references in dependency order; cancel callbacks before their stack-backed
// userdata goes out of scope. No mainloop dispatch occurs during destruction.
struct Client {
    mainloop: *mut pa::pa_mainloop,
    context: *mut pa::pa_context,
    operations: [*mut pa::pa_operation; 2],
}
impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: pointers are null or exclusively owned references from libpulse;
        // operations are released before their context, and context before loop.
        unsafe {
            for operation in self.operations {
                if !operation.is_null() {
                    pa::pa_operation_cancel(operation);
                    pa::pa_operation_unref(operation);
                }
            }
            if !self.context.is_null() {
                pa::pa_context_set_subscribe_callback(self.context, None, ptr::null_mut());
                pa::pa_context_disconnect(self.context);
                pa::pa_context_unref(self.context);
            }
            if !self.mainloop.is_null() {
                pa::pa_mainloop_free(self.mainloop);
            }
        }
    }
}

pub(super) fn snapshot(stop: &AtomicBool) -> Result<Snapshot, &'static str> {
    collect(&local_server()?, stop, DEADLINE)
}

fn local_server() -> Result<CString, &'static str> {
    // Explicit local Unix socket avoids DNS/remote connects on the diagnostic
    // worker. Sailfish Audio.permission exposes this user-session socket.
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            // SAFETY: getuid has no preconditions.
            std::path::PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }))
        });
    use std::os::unix::ffi::OsStrExt;
    let mut address = b"unix:".to_vec();
    address.extend_from_slice(runtime.join("pulse/native").as_os_str().as_bytes());
    CString::new(address).map_err(|_| "invalid local socket")
}

fn collect(server: &CStr, stop: &AtomicBool, timeout: Duration) -> Result<Snapshot, &'static str> {
    let started = Instant::now();
    check_budget(stop, started, timeout)?;
    // Declare userdata before its owner so cancellation/disconnect precedes its
    // destruction on every early-return path, including failed second query.
    let data = RefCell::new(Snapshot::default());
    let mut client = connect(server, stop, started, timeout)?;
    // SAFETY: native objects are exclusively accessed here on one thread. Each
    // callback receives a live, stable RefCell address only while collect dispatches.
    unsafe {
        let userdata = userdata(&data);
        client.operations[0] =
            pa::pa_context_get_sink_input_info_list(client.context, Some(input_callback), userdata);
        client.operations[1] =
            pa::pa_context_get_sink_info_list(client.context, Some(sink_callback), userdata);
    }
    if client.operations.iter().any(|p| p.is_null()) {
        return Err("query unavailable");
    }
    dispatch_until(&client, stop, started, timeout, "connection lost", || {
        let data = data.borrow();
        match data.error {
            Some(error) => Err(error),
            None => Ok(data.inputs_done && data.sinks_done),
        }
    })?;
    drop(client);
    Ok(data.into_inner())
}

fn connect(
    server: &CStr,
    stop: &AtomicBool,
    started: Instant,
    timeout: Duration,
) -> Result<Client, &'static str> {
    let mut client = Client {
        mainloop: ptr::null_mut(),
        context: ptr::null_mut(),
        operations: [ptr::null_mut(); 2],
    };
    // SAFETY: all allocated native objects are owned by Client on this thread.
    unsafe {
        client.mainloop = pa::pa_mainloop_new();
        if client.mainloop.is_null() {
            return Err("mainloop allocation");
        }
        client.context = pa::pa_context_new(
            pa::pa_mainloop_get_api(client.mainloop),
            c"PlexFreq diagnostics".as_ptr(),
        );
        if client.context.is_null() {
            return Err("context allocation");
        }
        if pa::pa_context_connect(
            client.context,
            server.as_ptr(),
            pa::PA_CONTEXT_NOAUTOSPAWN,
            ptr::null(),
        ) < 0
        {
            return Err("connect failed");
        }
    }
    dispatch_until(
        &client,
        stop,
        started,
        timeout,
        "connection denied/failed",
        || Ok(client.state() == pa::PA_CONTEXT_READY),
    )?;
    Ok(client)
}

impl Client {
    fn state(&self) -> pa::pa_context_state_t {
        // SAFETY: the context is live and confined to this worker.
        unsafe { pa::pa_context_get_state(self.context) }
    }
}

/// Erase a callback state cell for libpulse. Callbacks only reach it through
/// `RefCell`, so Rust-side reads never alias a callback's mutable access.
fn userdata<T>(cell: &RefCell<T>) -> *mut c_void {
    ptr::from_ref(cell).cast_mut().cast()
}

fn check_budget(
    stop: &AtomicBool,
    started: Instant,
    timeout: Duration,
) -> Result<(), &'static str> {
    if stop.load(Ordering::SeqCst) {
        Err("cancelled")
    } else if started.elapsed() >= timeout {
        Err("timeout")
    } else {
        Ok(())
    }
}

/// Longest single wait for libpulse events; bounds how late `stop` is noticed.
const POLL_SLICE: Duration = Duration::from_millis(50);

/// Dispatch until `ready` reports completion (or an error), the context fails
/// with `lost`, the budget expires or `stop` is set.
fn dispatch_until(
    client: &Client,
    stop: &AtomicBool,
    started: Instant,
    timeout: Duration,
    lost: &'static str,
    mut ready: impl FnMut() -> Result<bool, &'static str>,
) -> Result<(), &'static str> {
    loop {
        check_budget(stop, started, timeout)?;
        if ready()? {
            return Ok(());
        }
        if matches!(
            client.state(),
            pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED
        ) {
            return Err(lost);
        }
        iterate(
            client,
            POLL_SLICE.min(timeout.saturating_sub(started.elapsed())),
        )?;
    }
}

/// One mainloop iteration that sleeps in poll() until an event or `timeout`.
fn iterate(client: &Client, timeout: Duration) -> Result<(), &'static str> {
    let timeout = timeout.as_micros().min(i32::MAX as u128) as i32;
    // SAFETY: live, worker-confined loop. This is pa_mainloop_iterate split
    // into its documented steps so the wait is bounded instead of spinning.
    let failed = unsafe {
        pa::pa_mainloop_prepare(client.mainloop, timeout) < 0
            || pa::pa_mainloop_poll(client.mainloop) < 0
            || pa::pa_mainloop_dispatch(client.mainloop) < 0
    };
    if failed {
        Err("mainloop failed")
    } else {
        Ok(())
    }
}

/// Callback state of a libpulse list query.
trait ListState {
    const CALLBACK_FAILED: &'static str;
    fn error(&mut self) -> &mut Option<&'static str>;
}

/// Shared body of libpulse `*_info_list` callbacks: marks the list `done` at
/// its end (recording `failed` for an error end), passes each entry to
/// `entry`, and never lets a panic unwind into C.
///
/// # Safety
/// `userdata` must point to a live `RefCell<T>` and `info` must be null or
/// valid for the duration of the call.
unsafe fn list_callback<T: ListState, I>(
    info: *const I,
    eol: i32,
    userdata: *mut c_void,
    done: fn(&mut T) -> &mut bool,
    failed: &'static str,
    entry: fn(&mut T, &I) -> Result<(), &'static str>,
) {
    // SAFETY: guaranteed by the caller; only shared references are formed.
    let cell = unsafe { &*userdata.cast_const().cast::<RefCell<T>>() };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut data = cell.borrow_mut();
        if eol != 0 {
            *done(&mut data) = true;
            if eol < 0 {
                *data.error() = Some(failed);
            }
        // SAFETY: nonnull info is valid for this callback only.
        } else if let Some(info) = unsafe { info.as_ref() } {
            if let Err(error) = entry(&mut data, info) {
                *data.error() = Some(error);
            }
        }
    }));
    if result.is_err() {
        if let Ok(mut data) = cell.try_borrow_mut() {
            *data.error() = Some(T::CALLBACK_FAILED);
        }
    }
}

impl ListState for Snapshot {
    const CALLBACK_FAILED: &'static str = "callback failed";
    fn error(&mut self) -> &mut Option<&'static str> {
        &mut self.error
    }
}

// Only libpulse invokes these callbacks in production. No native pointer/string
// escapes its callback lifetime, and no panic may unwind through C.
extern "C" fn input_callback(
    _: *mut pa::pa_context,
    info: *const pa::pa_sink_input_info,
    eol: i32,
    userdata: *mut c_void,
) {
    // SAFETY: collect registers a live RefCell<Snapshot>; libpulse provides
    // callback-scoped info (or null for end/error).
    unsafe {
        list_callback(
            info,
            eol,
            userdata,
            |data: &mut Snapshot| &mut data.inputs_done,
            "input query denied/failed",
            input_entry,
        )
    }
}
fn input_entry(data: &mut Snapshot, info: &pa::pa_sink_input_info) -> Result<(), &'static str> {
    data.inputs += 1;
    if data.inputs > MAX_ENTRIES {
        return Err("entry limit");
    }
    // SAFETY: nonnull proplist is valid for the callback duration.
    if !info.proplist.is_null()
        && unsafe { property_equals(info.proplist, c"application.name", b"PlexFreq") }
    {
        data.streams.push(format!(
            "input={} sink={} corked={} mute={} sample={:?}/{}ch/{}Hz volume_pct={} buffer_latency_us={} sink_latency_us={}",
            info.index, info.sink, info.corked != 0, info.mute != 0,
            info.sample_spec.format, info.sample_spec.channels, info.sample_spec.rate,
            if info.has_volume != 0 { volume_range(&info.volume) } else { "unavailable".into() },
            info.buffer_usec, info.sink_usec));
    }
    Ok(())
}
extern "C" fn sink_callback(
    _: *mut pa::pa_context,
    info: *const pa::pa_sink_info,
    eol: i32,
    userdata: *mut c_void,
) {
    // SAFETY: same callback/userdata contract as input_callback.
    unsafe {
        list_callback(
            info,
            eol,
            userdata,
            |data: &mut Snapshot| &mut data.sinks_done,
            "sink query denied/failed",
            sink_entry,
        )
    }
}
fn sink_entry(data: &mut Snapshot, info: &pa::pa_sink_info) -> Result<(), &'static str> {
    if data.sinks.len() >= MAX_ENTRIES {
        return Err("entry limit");
    }
    let name = if info.name.is_null() {
        &[][..]
    } else {
        // SAFETY: name is NUL-terminated and valid for this callback.
        unsafe { CStr::from_ptr(info.name) }.to_bytes()
    };
    let kind = if name.windows(5).any(|s| s == b"bluez") {
        "bluetooth"
    } else if name.windows(4).any(|s| s == b"null") {
        "null"
    } else {
        "native/other"
    };
    data.sinks.push(format!(
        "sink={} kind={} state={:?} mute={} sample={:?}/{}ch/{}Hz volume_pct={} latency_us={} configured_latency_us={}",
        info.index, kind, info.state, info.mute != 0,
        info.sample_spec.format, info.sample_spec.channels, info.sample_spec.rate,
        volume_range(&info.volume), info.latency, info.configured_latency));
    Ok(())
}
unsafe fn property_equals(properties: *mut pa::pa_proplist, key: &CStr, expected: &[u8]) -> bool {
    // SAFETY: caller supplies a live proplist; returned string is borrowed only
    // within the callback. No arbitrary properties are copied into diagnostics.
    let value = unsafe { pa::pa_proplist_gets(properties, key.as_ptr()) };
    !value.is_null() && unsafe { CStr::from_ptr(value) }.to_bytes() == expected
}
fn volume_range(volume: &pa::pa_cvolume) -> String {
    let values = &volume.values[..usize::from(volume.channels).min(volume.values.len())];
    match (values.iter().min(), values.iter().max()) {
        (Some(min), Some(max)) => format!(
            "{}..{}",
            u64::from(*min) * 100 / u64::from(pa::PA_VOLUME_NORM),
            u64::from(*max) * 100 / u64::from(pa::PA_VOLUME_NORM)
        ),
        _ => "unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::net::UnixListener, sync::Arc, thread};

    #[test]
    fn silent_socket_times_out_and_shutdown_cancels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native");
        let _listener = UnixListener::bind(&path).unwrap();
        let server = CString::new(format!("unix:{}", path.display())).unwrap();
        let started = Instant::now();
        assert_eq!(
            collect(&server, &AtomicBool::new(false), Duration::from_millis(100)).unwrap_err(),
            "timeout"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            signal.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        assert_eq!(collect(&server, &stop, DEADLINE).unwrap_err(), "cancelled");
        assert!(started.elapsed() < Duration::from_secs(1));
        worker.join().unwrap();
        assert_eq!(collect(&server, &stop, DEADLINE).unwrap_err(), "cancelled");
    }

    #[test]
    fn missing_socket_fails_without_spawning_a_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing");
        let server = CString::new(format!("unix:{}", path.display())).unwrap();
        assert!(collect(&server, &AtomicBool::new(false), DEADLINE).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn native_callbacks_filter_private_fields_and_bound_records() {
        // SAFETY: these C POD structures have valid zero enum discriminants;
        // pointers are replaced with live fixture data before callbacks use them.
        unsafe {
            let properties = pa::pa_proplist_new();
            assert!(!properties.is_null());
            pa::pa_proplist_sets(properties, c"application.name".as_ptr(), c"Other".as_ptr());
            pa::pa_proplist_sets(
                properties,
                c"media.filename".as_ptr(),
                c"https://fixture.invalid/?X-Plex-Token=secret".as_ptr(),
            );
            let mut input: pa::pa_sink_input_info = std::mem::zeroed();
            input.proplist = properties;
            input.name = c"private title".as_ptr();
            input.index = 9;
            input.sink = 2;
            input.corked = 1;
            input.has_volume = 1;
            input.volume.channels = 2;
            input.volume.values[0] = pa::PA_VOLUME_NORM / 2;
            input.volume.values[1] = pa::PA_VOLUME_NORM;
            input.buffer_usec = 300000;
            let data = RefCell::new(Snapshot::default());
            let userdata = userdata(&data);
            input_callback(ptr::null_mut(), &input, 0, userdata);
            assert!(data.borrow().streams.is_empty());
            pa::pa_proplist_sets(
                properties,
                c"application.name".as_ptr(),
                c"PlexFreq".as_ptr(),
            );
            input_callback(ptr::null_mut(), &input, 0, userdata);
            input_callback(ptr::null_mut(), ptr::null(), 1, userdata);
            let mut sink: pa::pa_sink_info = std::mem::zeroed();
            sink.index = 2;
            sink.name = c"bluez_sink.private_address".as_ptr();
            sink.description = c"private car".as_ptr();
            sink_callback(ptr::null_mut(), &sink, 0, userdata);
            sink_callback(ptr::null_mut(), ptr::null(), 1, userdata);
            let summary = format!("{} {}", data.borrow().streams(), data.borrow().routes());
            assert!(summary.contains("inputs=2 plexfreq_streams=1"));
            assert!(summary.contains("input=9 sink=2 corked=true"));
            assert!(summary.contains("volume_pct=50..100"));
            assert!(summary.contains("buffer_latency_us=300000"));
            assert!(summary.contains("sink=2 kind=bluetooth"));
            for private in ["private", "secret", "fixture.invalid", "Other"] {
                assert!(!summary.contains(private));
            }
            assert!(data.borrow().inputs_done && data.borrow().sinks_done);
            for _ in 0..MAX_ENTRIES {
                input_callback(ptr::null_mut(), &input, 0, userdata);
            }
            let data = data.into_inner();
            assert_eq!(data.error, Some("entry limit"));
            assert!(data.streams.len() <= MAX_ENTRIES);
            assert!(data.streams().len() < 16 * 1024);
            pa::pa_proplist_free(properties);
        }
    }

    #[test]
    fn query_error_is_not_an_empty_success() {
        let data = RefCell::new(Snapshot::default());
        input_callback(ptr::null_mut(), ptr::null(), -1, userdata(&data));
        let data = data.into_inner();
        assert_eq!(data.error, Some("input query denied/failed"));
        assert!(data.inputs_done);
    }
}
