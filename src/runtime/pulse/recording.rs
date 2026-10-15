//! Observe microphone stream activity, never microphone samples. A persistent
//! read-only subscription avoids polling connections and works inside Audio.permission.
use super::*;
use std::{cell::Cell, collections::BTreeSet};

/// Longest idle wait; subscription events still wake the loop immediately.
const IDLE_SLICE: Duration = Duration::from_millis(100);

#[derive(Default)]
struct Recording {
    microphones: BTreeSet<u32>,
    outputs: BTreeSet<u32>,
    sources: usize,
    streams: usize,
    sources_done: bool,
    streams_done: bool,
    error: Option<&'static str>,
}
impl Recording {
    fn active(&self) -> bool {
        !self.microphones.is_disjoint(&self.outputs)
    }
}
impl ListState for Recording {
    const CALLBACK_FAILED: &'static str = "recording callback failed";
    fn error(&mut self) -> &mut Option<&'static str> {
        &mut self.error
    }
}

pub fn watch_recording(
    stop: &AtomicBool,
    mut observe: impl FnMut(bool),
) -> Result<(), &'static str> {
    // Both callback cells outlive Client, including every error path. Callbacks
    // and this function only share them through Cell/RefCell.
    let dirty = Cell::new(true);
    let data = RefCell::new(Recording::default());
    let mut client = connect(&local_server()?, stop, Instant::now(), DEADLINE)?;
    // SAFETY: stable worker-local userdata; only this mainloop dispatches it.
    let operation = unsafe {
        pa::pa_context_set_subscribe_callback(
            client.context,
            Some(changed),
            ptr::from_ref(&dirty).cast_mut().cast(),
        );
        pa::pa_context_subscribe(
            client.context,
            pa::PA_SUBSCRIPTION_MASK_SOURCE | pa::PA_SUBSCRIPTION_MASK_SOURCE_OUTPUT,
            None,
            ptr::null_mut(),
        )
    };
    if operation.is_null() {
        return Err("recording subscribe unavailable");
    }
    client.operations[0] = operation;
    dispatch_until(
        &client,
        stop,
        Instant::now(),
        DEADLINE,
        "recording connection lost",
        // SAFETY: the operation stays referenced by client until released below.
        || Ok(unsafe { pa::pa_operation_get_state(operation) } != pa::PA_OPERATION_RUNNING),
    )?;
    // SAFETY: release the completed operation exactly once.
    unsafe { pa::pa_operation_unref(operation) };
    client.operations[0] = ptr::null_mut();
    let mut refreshed = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        // Periodic reconciliation also recovers a missed subscription event.
        if dirty.get() || refreshed.elapsed() >= Duration::from_secs(2) {
            dirty.set(false);
            *data.borrow_mut() = Recording::default();
            // SAFETY: callback data stays live until operations are cancelled or
            // complete. Queries and subscription share one worker mainloop.
            unsafe {
                let userdata = userdata(&data);
                client.operations[0] =
                    pa::pa_context_get_source_info_list(client.context, Some(source), userdata);
                client.operations[1] = pa::pa_context_get_source_output_info_list(
                    client.context,
                    Some(stream),
                    userdata,
                );
            }
            if client.operations.iter().any(|p| p.is_null()) {
                return Err("recording query unavailable");
            }
            dispatch_until(
                &client,
                stop,
                Instant::now(),
                DEADLINE,
                "recording connection lost",
                || {
                    let data = data.borrow();
                    match data.error {
                        Some(error) => Err(error),
                        None => Ok(data.sources_done && data.streams_done),
                    }
                },
            )?;
            // SAFETY: no callbacks remain pending after both end-of-list callbacks.
            unsafe {
                for operation in &mut client.operations {
                    pa::pa_operation_cancel(*operation);
                    pa::pa_operation_unref(*operation);
                    *operation = ptr::null_mut();
                }
            }
            // Reconcile events arriving during the two lists before using a mixed
            // snapshot. No empty/failed query is interpreted as recording stopped.
            if !dirty.get() {
                observe(data.borrow().active());
            }
            refreshed = Instant::now();
        }
        if matches!(
            client.state(),
            pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED
        ) {
            return Err("recording connection lost");
        }
        // Sleep in poll() until an event arrives; the bound keeps stop and the
        // periodic reconciliation responsive (at most 10 idle wakes/s).
        iterate(&client, IDLE_SLICE)?;
    }
    Ok(())
}

extern "C" fn changed(
    _: *mut pa::pa_context,
    _: pa::pa_subscription_event_type_t,
    _: u32,
    userdata: *mut c_void,
) {
    // SAFETY: watch_recording owns this stable Cell until Client disconnects.
    unsafe { &*userdata.cast_const().cast::<Cell<bool>>() }.set(true);
}
extern "C" fn source(
    _: *mut pa::pa_context,
    info: *const pa::pa_source_info,
    eol: i32,
    userdata: *mut c_void,
) {
    // SAFETY: watch_recording registers a live RefCell<Recording>; libpulse
    // supplies callback-scoped info.
    unsafe {
        list_callback(
            info,
            eol,
            userdata,
            |data: &mut Recording| &mut data.sources_done,
            "recording sources query failed",
            source_entry,
        )
    }
}
fn source_entry(data: &mut Recording, info: &pa::pa_source_info) -> Result<(), &'static str> {
    data.sources += 1;
    if data.sources > MAX_ENTRIES {
        return Err("recording entry limit");
    }
    if info.monitor_of_sink == pa::PA_INVALID_INDEX {
        data.microphones.insert(info.index);
    }
    Ok(())
}
extern "C" fn stream(
    _: *mut pa::pa_context,
    info: *const pa::pa_source_output_info,
    eol: i32,
    userdata: *mut c_void,
) {
    // SAFETY: same callback-scoped lifetime contract as source.
    unsafe {
        list_callback(
            info,
            eol,
            userdata,
            |data: &mut Recording| &mut data.streams_done,
            "recording streams query failed",
            stream_entry,
        )
    }
}
fn stream_entry(
    data: &mut Recording,
    info: &pa::pa_source_output_info,
) -> Result<(), &'static str> {
    data.streams += 1;
    if data.streams > MAX_ENTRIES {
        return Err("recording entry limit");
    }
    if info.corked == 0 {
        data.outputs.insert(info.source);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_excludes_monitors_and_corked_streams_and_bounds_queries() {
        // SAFETY: valid C POD zero values, callback pointers remain in scope.
        unsafe {
            let data = RefCell::new(Recording::default());
            let userdata = userdata(&data);
            let mut microphone: pa::pa_source_info = std::mem::zeroed();
            microphone.index = 3;
            microphone.monitor_of_sink = pa::PA_INVALID_INDEX;
            source(ptr::null_mut(), &microphone, 0, userdata);
            let mut monitor: pa::pa_source_info = std::mem::zeroed();
            monitor.index = 4;
            monitor.monitor_of_sink = 1;
            source(ptr::null_mut(), &monitor, 0, userdata);
            let mut output: pa::pa_source_output_info = std::mem::zeroed();
            output.source = 4;
            stream(ptr::null_mut(), &output, 0, userdata);
            assert!(!data.borrow().active());
            output.source = 3;
            output.corked = 1;
            stream(ptr::null_mut(), &output, 0, userdata);
            assert!(!data.borrow().active());
            output.corked = 0;
            stream(ptr::null_mut(), &output, 0, userdata);
            assert!(data.borrow().active());
            for _ in 0..MAX_ENTRIES {
                stream(ptr::null_mut(), &output, 0, userdata);
            }
            let data = data.into_inner();
            assert_eq!(data.error, Some("recording entry limit"));
            assert!(data.outputs.len() <= MAX_ENTRIES);
        }
    }
    #[test]
    fn failed_lists_are_unavailable_not_empty_success() {
        let data = RefCell::new(Recording::default());
        source(ptr::null_mut(), ptr::null(), -1, userdata(&data));
        let data = data.into_inner();
        assert!(data.sources_done);
        assert!(data.error.is_some());
    }
    #[test]
    fn subscription_events_mark_the_shared_flag_dirty() {
        let dirty = Cell::new(false);
        changed(
            ptr::null_mut(),
            0,
            0,
            ptr::from_ref(&dirty).cast_mut().cast(),
        );
        assert!(dirty.get());
    }
}
