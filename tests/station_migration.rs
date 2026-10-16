//! Startup wiring for scoped station download groups and offline-store health.

use plexfreq_core::{cache, Command, Core};
use serde_json::{json, Value};
use std::fs;

const STATION: &str = "/library/sections/1/station/1";

fn legacy_session(dir: &std::path::Path) -> String {
    let legacy = format!("station:{}", cache::namespace(STATION, "station"));
    let session = json!({
        "clientIdentifier": "fixture-client",
        "serverUrl": "http://127.0.0.1:9/",
        "serverToken": "fixture-not-a-token",
        "downloadGroups": {legacy.clone(): ["1", "2"]},
        "downloadTitles": {legacy.clone(): "Library radio"},
        "downloadPlans": {
            legacy.clone(): {"kind":"station","key":STATION,"minutes":30,"refreshedAt":1}
        }
    });
    fs::write(dir.join("session.json"), session.to_string()).unwrap();
    legacy
}

fn groups(dir: &std::path::Path) -> Value {
    let session: Value =
        serde_json::from_slice(&fs::read(dir.join("session.json")).unwrap()).unwrap();
    session["downloadGroups"].clone()
}

// Legacy `station:<hash(path)>` groups collide across servers/accounts. The
// owning Core migrates them to namespace-scoped ids when it starts.
#[test]
fn owner_startup_migrates_legacy_station_groups() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = legacy_session(dir.path());
    drop(Core::new(dir.path().into()).unwrap());
    let groups = groups(dir.path());
    let keys: Vec<&String> = groups.as_object().unwrap().keys().collect();
    assert_eq!(keys.len(), 1, "{groups}");
    assert_ne!(keys[0], &legacy, "legacy id must be migrated");
    assert!(keys[0].starts_with("station:"));
    assert_eq!(groups[keys[0]], json!(["1", "2"]));
}

// Read-only inspection (the refresh worker) never rewrites session files.
#[test]
fn inspection_does_not_migrate_station_groups() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = legacy_session(dir.path());
    let before = fs::read(dir.path().join("session.json")).unwrap();
    drop(Core::inspect(dir.path().into()).unwrap());
    assert_eq!(before, fs::read(dir.path().join("session.json")).unwrap());
    assert!(groups(dir.path()).get(&legacy).is_some());
}

// Lyric/snapshot save failures must be readable by the UI, not only logged.
#[test]
fn cache_status_reports_offline_store_health() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    let status = core.execute(Command::CacheStatus).unwrap();
    assert_eq!(status["cache"]["offlineStore"]["ok"], true, "{status}");
    assert_eq!(status["cache"]["offlineStore"]["failures"], 0);
}
