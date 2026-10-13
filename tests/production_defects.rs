//! Production-review reproductions: each test asserts the *correct*
//! production behaviour and therefore FAILS until the defect is fixed.
//! Do not weaken these assertions to make CI green; fix the source instead.
use plexfreq_core::{Command, Core};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Server {
    fn new(handler: impl Fn(&str) -> Value + Send + 'static) -> Self {
        Self::bytes(move |r| ("application/json", handler(r).to_string().into_bytes()))
    }
    fn bytes(handler: impl Fn(&str) -> (&'static str, Vec<u8>) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut buffer = [0; 4096];
                        while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                            let n = stream.read(&mut buffer).unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&buffer[..n]);
                        }
                        if bytes.is_empty() {
                            continue;
                        }
                        let request = String::from_utf8(bytes).unwrap();
                        let (mime, body) = handler(&request);
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        Self {
            url,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn call(core: &mut Core, input: Value) -> plexfreq_core::Result<Value> {
    core.execute(serde_json::from_value::<Command>(input).unwrap())
}
fn track(id: &str) -> Value {
    json!({"ratingKey":id,"key":format!("/library/metadata/{id}"),"type":"track","title":format!("Track {id}"),"duration":180000,"Media":[{"audioCodec":"flac","Part":[{"key":format!("/library/parts/{id}/audio.flac")}]}]})
}
fn connect(core: &mut Core, server: &Server) {
    call(
        core,
        json!({"op":"download_policy","wifi_only":false,"paused":true}),
    )
    .unwrap();
    call(
        core,
        json!({"op":"connect","url":server.url,"token":"fixture"}),
    )
    .unwrap();
}
fn empty() -> Value {
    json!({"MediaContainer":{"Directory":[],"Metadata":[]}})
}

/// src/model.rs `lenient_f64` accepts `"NaN"`/`"inf"` via `str::parse`,
/// unlike `optional_gain` which filters non-finite values. A single bad
/// `userRating` string then makes `display_item`'s `to_value` fail and the
/// whole track DTO collapses to `{}` (src/lib.rs `display_item`).
#[test]
fn user_rating_nonfinite_string_must_not_poison_track_dto() {
    for raw in ["NaN", "inf", "-inf", "Infinity", "NAN"] {
        let payload = json!({
            "ratingKey": "1",
            "key": "/library/metadata/1",
            "type": "track",
            "title": "Track 1",
            "userRating": raw,
        });
        let item: plexfreq_core::model::Item = serde_json::from_value(payload).unwrap();
        assert!(
            item.user_rating.is_finite(),
            "userRating {raw:?} must deserialize to a finite default, got {}",
            item.user_rating
        );
        assert_eq!(item.user_rating, 0.0);
        // JSON cannot represent NaN/inf: this errors while the defect is present.
        assert!(
            serde_json::to_value(&item).is_ok(),
            "track with userRating {raw:?} must remain serializable for display_item"
        );
    }
    // Numeric JSON can never be non-finite, so only the string path is affected.
    let numeric = json!({
        "ratingKey": "1",
        "key": "/library/metadata/1",
        "type": "track",
        "title": "Track 1",
        "userRating": 8.5,
    });
    let item: plexfreq_core::model::Item = serde_json::from_value(numeric).unwrap();
    assert_eq!(item.user_rating, 8.5);
}

/// src/filters.rs `smart_rules` maps an unparseable `limit` to `0` via
/// `unwrap_or(0)`, then emits `smartLimit: 0` which violates the `1..=1000`
/// contract checked on the same line and enforced by `smart_playlist`.
#[test]
fn smart_limit_invalid_must_not_emit_zero() {
    let server = Server::new(|r| {
        if r.starts_with("GET /playlists/5") {
            let content = "server://fixture-server/com.plexapp.plugins.library/library/sections/1/all?type=10&limit=bad&sort=titleSort%3Aasc";
            return json!({"MediaContainer":{"Metadata":[{"type":"playlist","ratingKey":"5","smart":true,"content":content}]}});
        }
        empty()
    });
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    connect(&mut core, &server);
    let rules = call(&mut core, json!({"op":"smart_rules","key":"5"})).unwrap();
    assert_eq!(
        rules["smartEditable"], false,
        "unparseable limit must not be editable"
    );
    let limit = rules["smartLimit"].as_u64().unwrap();
    assert!(
        (1..=1000).contains(&limit),
        "smartLimit must stay within 1..=1000 even when unparseable, got {limit}"
    );
}

/// src/cache.rs `Cache::build` returns `Err` on whole-file `index.json`
/// syntax failure, so one corrupt write bricks every later `Core::new`.
/// Per-entry corruption is already tolerated (`manifest_entries`); the index
/// itself must degrade to an empty manifest (preserving the corrupt copy for
/// evidence) instead of refusing startup.
#[test]
fn corrupt_cache_index_must_recover_not_brick_startup() {
    let dir = tempfile::tempdir().unwrap();
    let cache_dir = dir.path().join("audio-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(cache_dir.join("index.json"), b"{not valid json").unwrap();
    // Must succeed with an empty usable catalogue, not propagate the error.
    let cache = plexfreq_core::cache::Cache::new(
        cache_dir.clone(),
        plexfreq_core::cache::Config::default(),
        "client",
    )
    .expect("corrupt audio cache index must not brick startup");
    assert_eq!(cache.items("ns").len(), 0);
    let status = cache.status("ns");
    assert!(status.get("bytes").is_some());
}

/// src/store.rs `load` maps any `index.json`-style syntax failure to
/// `Session file is corrupt`, and `Core::build` propagates it, so one bad
/// byte loses all settings and blocks startup. Must fall back to defaults
/// (preserving the corrupt copy) instead.
#[test]
fn corrupt_session_must_recover_not_brick_startup() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("session.json"), b"{corrupt").unwrap();
    let settings =
        plexfreq_core::store::load(dir.path()).expect("corrupt session must fall back to default");
    assert!(settings.client_identifier.is_empty());
    // Full Core startup must also survive the corrupt session file.
    let mut core = Core::new(dir.path().into()).expect("Core::new must survive corrupt session");
    let status: Value = core
        .execute(serde_json::from_value::<Command>(json!({"op":"status"})).unwrap())
        .unwrap();
    assert!(status.is_object());
}

/// src/queue.rs `reorder` (non-shuffled path) retains a dangling `current`
/// (`Some(out-of-bounds)`) via `unwrap_or(preserved)`, while the shuffled
/// path drops it to `None`. `track()` then returns `None` while
/// `current.is_some()`, an inconsistent state that `validate` would reject.
#[test]
fn queue_reorder_must_drop_dangling_current() {
    let a: plexfreq_core::model::Item = serde_json::from_value(track("1")).unwrap();
    let b: plexfreq_core::model::Item = serde_json::from_value(track("2")).unwrap();
    let mut queue = plexfreq_core::queue::Queue::default();
    queue.replace(vec![a, b], 0).unwrap();
    // Corrupt the persisted selection without touching the valid order.
    queue.current = Some(99);
    // Non-shuffled reorder must not retain the out-of-bounds selection.
    queue.shuffle(false);
    assert_eq!(
        queue.current, None,
        "dangling current must be dropped, not retained as Some(99)"
    );
    assert!(queue.track().is_none());
}

/// src/queue.rs `trim_history` early-returns when `current.is_none()`, so
/// after a natural queue end (`advance` sets `current=None` while retaining
/// the last slot) radio history is never trimmed and `append` can grow to
/// `MAX_QUEUE` before the next play.
#[test]
fn queue_trim_history_must_work_after_natural_end() {
    let items: Vec<plexfreq_core::model::Item> = (1..=5)
        .map(|i| serde_json::from_value(track(&i.to_string())).unwrap())
        .collect();
    let mut queue = plexfreq_core::queue::Queue::default();
    queue.replace(items, 0).unwrap();
    // Advance to the natural end: last slot retained, no current selection.
    for _ in 0..5 {
        queue.advance(false);
    }
    assert!(queue.track().is_none());
    let before = queue.items.len();
    queue.trim_history(2);
    assert!(
        queue.items.len() < before,
        "trim_history after natural end must drop old history (was {before}, now {})",
        queue.items.len()
    );
}

/// src/audio/dsp.rs `crossfade` silently leaves `output[len..]` as
/// caller-supplied stale data when inputs differ in length. The steady-state
/// caller passes equal blocks, so a future length mismatch becomes a silent
/// gap/click instead of an explicit zero-fill or error.
#[test]
fn crossfade_mismatched_lengths_must_not_leave_stale_tail() {
    let old = [1.0_f32; 8];
    let new = [0.0_f32; 4];
    let mut output = [9.0_f32; 8];
    plexfreq_core::audio::dsp::crossfade(&old, &new, &mut output, 0, 2);
    assert!(
        output[4..].iter().all(|s| *s == 0.0),
        "unmixed tail must be zeroed, not left stale: {output:?}"
    );
}

/// src/audio/dsp.rs default `headroom_db = 0` hard-clips the equal-power
/// crossfade centre peak (sqrt(2) ~= 1.414) to 1.0 via `clamp(-1, 1)`,
/// producing audible distortion on every centred crossfade. Either the
/// default headroom or a soft limiter must preserve the peak.
#[test]
fn default_processor_must_not_hard_clip_equal_power_peak() {
    let mut processor = plexfreq_core::audio::dsp::Processor::new(
        48000,
        &plexfreq_core::audio::dsp::Config::default(),
    );
    let mut pcm = [std::f32::consts::SQRT_2; 2];
    processor.process(&mut pcm, 0.0, 1.0);
    assert!(
        (pcm[0] - std::f32::consts::SQRT_2).abs() < 1e-3,
        "default headroom must preserve equal-power peak, got {:?}",
        pcm
    );
}
