//! Observe microphone stream activity, never microphone samples. A persistent
//! read-only subscription avoids polling connections and works inside Audio.permission.
use super::*;
use std::collections::BTreeSet;

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

pub fn watch_recording(
    stop: &AtomicBool,
    mut observe: impl FnMut(bool),
) -> Result<(), &'static str> {
    // Both callback addresses outlive Client, including every error path.
    let mut dirty = true;
    let mut data: Recording;
    let mut client = connect(&local_server()?, stop, Instant::now(), DEADLINE)?;
    // SAFETY: stable worker-local userdata; only this mainloop dispatches it.
    unsafe {
        pa::pa_context_set_subscribe_callback(
            client.context,
            Some(changed),
            (&mut dirty as *mut bool).cast(),
        );
        let operation = pa::pa_context_subscribe(
            client.context,
            pa::PA_SUBSCRIPTION_MASK_SOURCE | pa::PA_SUBSCRIPTION_MASK_SOURCE_OUTPUT,
            None,
            ptr::null_mut(),
        );
        if operation.is_null() {
            return Err("recording subscribe unavailable");
        }
        client.operations[0] = operation;
        let started = Instant::now();
        while pa::pa_operation_get_state(operation) == pa::PA_OPERATION_RUNNING {
            check_budget(stop, started, DEADLINE)?;
            iterate(&client)?;
        }
        pa::pa_operation_unref(operation);
        client.operations[0] = ptr::null_mut();
    }
    let mut refreshed = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        // Periodic reconciliation also recovers a missed subscription event.
        if dirty || refreshed.elapsed() >= Duration::from_secs(2) {
            dirty = false;
            data = Recording::default();
            // SAFETY: callback data stays live until operations are cancelled or
            // complete. Queries and subscription share one nonblocking mainloop.
            unsafe {
                let userdata = (&mut data as *mut Recording).cast();
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
            let started = Instant::now();
            loop {
                check_budget(stop, started, DEADLINE)?;
                if let Some(error) = data.error {
                    return Err(error);
                }
                if data.sources_done && data.streams_done {
                    break;
                }
                iterate(&client)?;
            }
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
            if !dirty {
                observe(data.active());
            }
            refreshed = Instant::now();
        }
        // SAFETY: context remains owned and confined to this worker.
        if matches!(
            unsafe { pa::pa_context_get_state(client.context) },
            pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED
        ) {
            return Err("recording connection lost");
        }
        iterate(&client)?;
        // Keep an idle observer at 10 wakes/s rather than the connection/query
        // loop's 100 wakes/s; recording events are still handled within ~100 ms.
        thread::sleep(Duration::from_millis(90));
    }
    Ok(())
}

extern "C" fn changed(
    _: *mut pa::pa_context,
    _: pa::pa_subscription_event_type_t,
    _: u32,
    userdata: *mut c_void,
) {
    // SAFETY: watch_recording owns this stable bool until Client disconnects.
    unsafe {
        *userdata.cast::<bool>() = true;
    }
}
extern "C" fn source(
    _: *mut pa::pa_context,
    info: *const pa::pa_source_info,
    eol: i32,
    userdata: *mut c_void,
) {
    // SAFETY: libpulse supplies callback-scoped info and live worker-local data.
    let data = unsafe { &mut *userdata.cast::<Recording>() };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if eol != 0 {
            data.sources_done = true;
            if eol < 0 {
                data.error = Some("recording sources query failed");
            }
        } else if !info.is_null() {
            data.sources += 1;
            if data.sources > MAX_ENTRIES {
                data.error = Some("recording entry limit");
                return;
            }
            // SAFETY: nonnull info is valid for this callback only.
            let info = unsafe { &*info };
            if info.monitor_of_sink == pa::PA_INVALID_INDEX {
                data.microphones.insert(info.index);
            }
        }
    }))
    .is_err()
    {
        data.error = Some("recording callback failed");
    }
}
extern "C" fn stream(
    _: *mut pa::pa_context,
    info: *const pa::pa_source_output_info,
    eol: i32,
    userdata: *mut c_void,
) {
    // SAFETY: same callback-scoped lifetime contract as source.
    let data = unsafe { &mut *userdata.cast::<Recording>() };
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if eol != 0 {
            data.streams_done = true;
            if eol < 0 {
                data.error = Some("recording streams query failed");
            }
        } else if !info.is_null() {
            data.streams += 1;
            if data.streams > MAX_ENTRIES {
                data.error = Some("recording entry limit");
                return;
            }
            // SAFETY: nonnull info is valid for this callback only.
            let info = unsafe { &*info };
            if info.corked == 0 {
                data.outputs.insert(info.source);
            }
        }
    }))
    .is_err()
    {
        data.error = Some("recording callback failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_excludes_monitors_and_corked_streams_and_bounds_queries() {
        // SAFETY: valid C POD zero values, callback pointers remain in scope.
        unsafe {
            let mut data = Recording::default();
            let userdata = (&mut data as *mut Recording).cast();
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
            assert!(!data.active());
            output.source = 3;
            output.corked = 1;
            stream(ptr::null_mut(), &output, 0, userdata);
            assert!(!data.active());
            output.corked = 0;
            stream(ptr::null_mut(), &output, 0, userdata);
            assert!(data.active());
            for _ in 0..MAX_ENTRIES {
                stream(ptr::null_mut(), &output, 0, userdata);
            }
            assert_eq!(data.error, Some("recording entry limit"));
            assert!(data.outputs.len() <= MAX_ENTRIES);
        }
    }
    #[test]
    fn failed_lists_are_unavailable_not_empty_success() {
        let mut data = Recording::default();
        source(
            ptr::null_mut(),
            ptr::null(),
            -1,
            (&mut data as *mut Recording).cast(),
        );
        assert!(data.sources_done);
        assert!(data.error.is_some());
    }
}
