use crate::{Command, Core};
use serde_json::json;
use std::{
    ffi::{CStr, CString},
    os::raw::c_char,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    ptr,
};

fn output(value: serde_json::Value) -> *mut c_char {
    CString::new(value.to_string()).unwrap().into_raw()
}

/// # Safety
/// `dir` is a valid NUL-terminated UTF-8 string. Runtime is owned by its GUI bridge.
#[no_mangle]
pub unsafe extern "C" fn pf_runtime_new(dir: *const c_char) -> *mut crate::runtime::Runtime {
    if dir.is_null() {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let path = unsafe { CStr::from_ptr(dir) }.to_str().ok()?;
        crate::runtime::Runtime::new(PathBuf::from(path))
            .ok()
            .map(Box::new)
            .map(Box::into_raw)
    })
    .ok()
    .flatten()
    .unwrap_or(ptr::null_mut())
}
/// # Safety
/// `runtime` is live, exclusively used by its bridge; request is valid UTF-8 C text.
#[no_mangle]
pub unsafe extern "C" fn pf_runtime_submit(
    runtime: *mut crate::runtime::Runtime,
    request: *const c_char,
) -> *mut c_char {
    if runtime.is_null() || request.is_null() {
        return output(json!({"accepted":false,"error":"Backend unavailable"}));
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let input = unsafe { CStr::from_ptr(request) }
            .to_str()
            .map_err(|_| crate::Error::Input("Invalid UTF-8 request"))?;
        unsafe { &*runtime }.submit(serde_json::from_str(input)?)
    }));
    output(match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => json!({"accepted":false,"error":error.to_string()}),
        Err(_) => json!({"accepted":false,"error":"Backend panic"}),
    })
}
/// # Safety
/// `runtime` is live and exclusively polled by its bridge. Free returned C text.
#[no_mangle]
pub unsafe extern "C" fn pf_runtime_poll(runtime: *mut crate::runtime::Runtime) -> *mut c_char {
    if runtime.is_null() {
        return output(json!({"events":[],"error":"Backend unavailable"}));
    }
    output(
        catch_unwind(AssertUnwindSafe(|| unsafe { &*runtime }.poll()))
            .unwrap_or_else(|_| json!({"events":[],"error":"Backend panic"})),
    )
}
/// # Safety
/// Free one live GUI runtime with no concurrent GUI calls. Rust joins its workers.
#[no_mangle]
pub unsafe extern "C" fn pf_runtime_free(runtime: *mut crate::runtime::Runtime) {
    if !runtime.is_null() {
        drop(unsafe { Box::from_raw(runtime) });
    }
}

/// Create a thread-confined core. Returns null on invalid path or initialization failure.
/// # Safety
/// `dir` must be a valid NUL-terminated UTF-8 string for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn pf_new(dir: *const c_char) -> *mut Core {
    if dir.is_null() {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let path = unsafe { CStr::from_ptr(dir) }.to_str().ok()?;
        Core::new(PathBuf::from(path))
            .ok()
            .map(Box::new)
            .map(Box::into_raw)
    })
    .ok()
    .flatten()
    .unwrap_or(ptr::null_mut())
}

/// Create a diagnostic core without media prefetch/recovery.
/// # Safety
/// `dir` must be a valid NUL-terminated UTF-8 string for the duration of this call.
#[no_mangle]
pub unsafe extern "C" fn pf_new_inspect(dir: *const c_char) -> *mut Core {
    if dir.is_null() {
        return ptr::null_mut();
    }
    catch_unwind(|| {
        let path = unsafe { CStr::from_ptr(dir) }.to_str().ok()?;
        Core::inspect(PathBuf::from(path))
            .ok()
            .map(Box::new)
            .map(Box::into_raw)
    })
    .ok()
    .flatten()
    .unwrap_or(ptr::null_mut())
}

/// Clone a cancellation-only handle while Core is exclusively borrowed.
/// # Safety
/// `core` must be a live, exclusively borrowed handle. Free the result once.
#[no_mangle]
pub unsafe extern "C" fn pf_download_control(
    core: *mut Core,
) -> *mut crate::cache::DownloadControl {
    if core.is_null() {
        return ptr::null_mut();
    }
    catch_unwind(AssertUnwindSafe(|| {
        Box::into_raw(Box::new(unsafe { &*core }.download_control()))
    }))
    .unwrap_or(ptr::null_mut())
}
/// # Safety
/// `control` must be live and not concurrently freed. May run alongside pf_call.
#[no_mangle]
pub unsafe extern "C" fn pf_download_network(
    control: *mut crate::cache::DownloadControl,
    wifi: i32,
) {
    if !control.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| unsafe { &*control }.network(wifi != 0)));
    }
}
/// # Safety
/// `control` must be live and not concurrently freed. May run alongside pf_call.
#[no_mangle]
pub unsafe extern "C" fn pf_download_policy(
    control: *mut crate::cache::DownloadControl,
    wifi_only: i32,
    paused: i32,
) {
    if !control.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            unsafe { &*control }.policy(wifi_only != 0, paused != 0)
        }));
    }
}
/// # Safety
/// Free one live cancellation handle, with no concurrent hint calls.
#[no_mangle]
pub unsafe extern "C" fn pf_download_control_free(control: *mut crate::cache::DownloadControl) {
    if !control.is_null() {
        drop(unsafe { Box::from_raw(control) });
    }
}

/// Execute one JSON command and return an owned JSON envelope.
/// # Safety
/// `core` must be a live pf_new handle, exclusively borrowed; `request` a valid C string.
#[no_mangle]
pub unsafe extern "C" fn pf_call(core: *mut Core, request: *const c_char) -> *mut c_char {
    if core.is_null() || request.is_null() {
        return output(json!({"ok": false, "error": "Backend unavailable"}));
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let text = unsafe { CStr::from_ptr(request) }
            .to_str()
            .map_err(|_| crate::Error::Input("Invalid UTF-8 request"))?;
        let command: Command = serde_json::from_str(text)?;
        unsafe { &mut *core }.execute(command)
    }));
    output(match result {
        Ok(Ok(data)) => json!({"ok": true, "data": data}),
        Ok(Err(error)) => json!({"ok": false, "error": error.to_string()}),
        Err(_) => json!({"ok": false, "error": "Backend panic"}),
    })
}

/// # Safety
/// Pass a live handle from pf_new exactly once, with no concurrent calls.
#[no_mangle]
pub unsafe extern "C" fn pf_free(core: *mut Core) {
    if !core.is_null() {
        drop(unsafe { Box::from_raw(core) });
    }
}

/// # Safety
/// Pass a live string from pf_call exactly once.
#[no_mangle]
pub unsafe extern "C" fn pf_string_free(value: *mut c_char) {
    if !value.is_null() {
        drop(unsafe { CString::from_raw(value) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_handles_invalid_input_and_paired_ownership() {
        unsafe {
            assert!(pf_new(ptr::null()).is_null());
            let dir = tempfile::tempdir().unwrap();
            let path = CString::new(dir.path().to_str().unwrap()).unwrap();
            let core = pf_new(path.as_ptr());
            assert!(!core.is_null());
            for request in ["not-json", "{\"op\":\"unknown\"}", "{\"op\":\"status\"}"] {
                let request = CString::new(request).unwrap();
                let value = pf_call(core, request.as_ptr());
                let response: serde_json::Value =
                    serde_json::from_slice(CStr::from_ptr(value).to_bytes()).unwrap();
                assert_eq!(response["ok"], request.to_bytes().ends_with(b"status\"}"));
                pf_string_free(value);
            }
            let response = pf_call(core, ptr::null());
            assert!(CStr::from_ptr(response)
                .to_str()
                .unwrap()
                .contains("Backend unavailable"));
            pf_string_free(response);
            let control = pf_download_control(core);
            assert!(!control.is_null());
            pf_download_policy(control, 1, 0);
            pf_download_network(control, 0);
            pf_free(core);
            // The cancellation clone has its own Arc ownership, not a Core borrow.
            pf_download_network(control, 1);
            pf_download_control_free(control);
            pf_download_network(ptr::null_mut(), 0);
            pf_download_control_free(ptr::null_mut());
            pf_free(ptr::null_mut());
            pf_string_free(ptr::null_mut());
        }
    }
}
