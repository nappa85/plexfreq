use super::*;
use crate::{discovery::Listen, history::Play};
use std::os::unix::fs::MetadataExt;

const NS: &str = "0123456789abcdef0123456789abcdef";

fn listen(i: usize) -> Listen {
    Listen {
        namespace: NS.into(),
        key: format!("{}", 100000 + i),
        occurrence: format!("00000000-0000-4000-8000-{i:012}"),
        played_at: 1_700_000_000 + i as u64,
        listened_ms: 200000,
        title: format!("Track title number {i} of a long album"),
        artist: "Some Artist Name".into(),
        album: "Some Album Title (Deluxe Remastered Edition)".into(),
        artist_key: "4000".into(),
        album_key: "5000".into(),
    }
}

fn play(i: usize, delivered: bool) -> Play {
    Play {
        namespace: NS.into(),
        key: format!("{}", 100000 + i),
        occurrence: format!("00000000-0000-4000-8000-{i:012}"),
        played_at: 1_700_000_000 + i as u64,
        delivered,
    }
}

fn item(i: usize) -> crate::model::Item {
    crate::model::Item {
        rating_key: format!("{}", 100000 + i),
        key: format!("/library/metadata/{}", 100000 + i),
        title: format!("Track title number {i} of a long album"),
        kind: crate::model::ItemKind::Track,
        parent_title: "Some Album Title (Deluxe Remastered Edition)".into(),
        grandparent_title: "Some Artist Name".into(),
        thumb: format!("/library/metadata/{}/thumb/1700000000", 100000 + i),
        parent_thumb: "/library/metadata/5000/thumb/1700000000".into(),
        art: "/library/metadata/4000/art/1700000000".into(),
        summary: "x".repeat(200),
        duration: 240000,
        parent_rating_key: "5000".into(),
        grandparent_rating_key: "4000".into(),
        ..Default::default()
    }
}

/// Settings shaped like a long-lived session at the persistence caps.
fn synthetic(listens: usize, history: usize, queue: usize) -> Settings {
    let mut s = Settings {
        client_identifier: "fixture-client".into(),
        ..Default::default()
    };
    s.listens = (0..listens).map(listen).collect();
    s.history = (0..history).map(|i| play(i, i % 2 == 0)).collect();
    if queue > 0 {
        s.playback
            .queue
            .replace((0..queue).map(item).collect(), 0)
            .unwrap();
        s.playback.occurrence = "00000000-0000-4000-8000-000000000000".into();
        s.playback.position = 1234;
        s.playback.generation = 7;
    }
    s
}

/// Everything a caller can observe after loading.
fn snapshot(s: &Settings) -> serde_json::Value {
    serde_json::json!({
        "session": serde_json::to_value(s).unwrap(),
        "queue": serde_json::to_value(&s.playback.queue).unwrap(),
        "radio": serde_json::to_value(&s.playback.radio).unwrap(),
        "listens": serde_json::to_value(&s.listens).unwrap(),
        "history": serde_json::to_value(&s.history).unwrap(),
    })
}

fn lines(dir: &Path, file: &str) -> usize {
    fs::read(dir.join(file))
        .map(|b| b.iter().filter(|&&c| c == b'\n').count())
        .unwrap_or(0)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn ino(dir: &Path, file: &str) -> u64 {
    fs::metadata(dir.join(file)).unwrap().ino()
}

const LEGACY: &[u8] = include_bytes!("../../tests/fixtures/legacy-session.json");

#[test]
fn session_is_private_and_stale_temps_are_removed() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        ".session-1.tmp",
        "session.json.2.tmp",
        "keep.tmp",
        "queue.json.3.tmp",
        "listens.jsonl.4.tmp",
        "history.jsonl.5.tmp",
        "queue.json.corrupt-6.7.tmp",
    ] {
        fs::write(dir.path().join(name), b"secret").unwrap();
    }
    save(dir.path(), &Settings::default()).unwrap();
    let mode = fs::metadata(dir.path().join("session.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    remove_stale_temps(dir.path());
    assert_eq!(names(dir.path()), ["keep.tmp", "session.json"]);
}

#[test]
fn every_session_file_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    save(&state, &synthetic(2, 2, 2)).unwrap();
    // Appends create the log with the same mode.
    fs::remove_file(state.join(LISTENS)).unwrap();
    let mut s2 = load(&state).unwrap();
    s2.listens.push_back(listen(9));
    save_journals(&state, &s2).unwrap();
    for name in [SESSION, QUEUE, LISTENS, HISTORY] {
        let mode = fs::metadata(state.join(name)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{name}");
    }
    let mode = fs::metadata(&state).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn old_format_session_loads_identically_and_migrates_once() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(SESSION), LEGACY).unwrap();
    // What the previous implementation returned for this file.
    let expected: Settings = serde_json::from_slice(LEGACY).unwrap();
    assert_eq!(expected.listens.len(), 3);
    assert_eq!(expected.history.len(), 2);
    assert_eq!(expected.playback.queue.items().len(), 3);
    let loaded = load(dir.path()).unwrap();
    assert_eq!(snapshot(&loaded), snapshot(&expected));
    // Loading alone never migrates.
    assert_eq!(names(dir.path()), [SESSION]);
    assert!(needs_maintenance(&loaded));

    save(dir.path(), &loaded).unwrap();
    let session: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join(SESSION)).unwrap()).unwrap();
    assert!(session.get("listens").is_none());
    assert!(session.get("history").is_none());
    assert!(session["playback"].get("queue").is_none());
    assert!(session["playback"].get("radio").is_none());
    assert_eq!(session["playback"]["position"], 1234);
    assert_eq!(lines(dir.path(), LISTENS), 3);
    assert_eq!(lines(dir.path(), HISTORY), 2);
    let reloaded = load(dir.path()).unwrap();
    assert_eq!(snapshot(&reloaded), snapshot(&expected));
    assert!(!needs_maintenance(&reloaded));
}

#[test]
fn crash_between_new_files_and_session_rewrite_deduplicates() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(SESSION), LEGACY).unwrap();
    let expected: Settings = serde_json::from_slice(LEGACY).unwrap();
    save(dir.path(), &load(dir.path()).unwrap()).unwrap();
    // Simulate a crash before the old session.json was replaced.
    fs::write(dir.path().join(SESSION), LEGACY).unwrap();
    let loaded = load(dir.path()).unwrap();
    assert_eq!(snapshot(&loaded), snapshot(&expected));
    assert!(needs_maintenance(&loaded));
    save(dir.path(), &loaded).unwrap();
    assert_eq!(lines(dir.path(), LISTENS), 3);
    assert_eq!(snapshot(&load(dir.path()).unwrap()), snapshot(&expected));
}

#[test]
fn mutations_append_one_line_and_newest_record_wins() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = load(dir.path()).unwrap();
    s.client_identifier = "fixture-client".into();
    save(dir.path(), &s).unwrap();
    s.listens.push_back(listen(1));
    s.history.push(play(1, false));
    save_journals(dir.path(), &s).unwrap();
    assert_eq!(lines(dir.path(), LISTENS), 1);
    assert_eq!(lines(dir.path(), HISTORY), 1);
    s.listens[0].listened_ms = 250000;
    save_journals(dir.path(), &s).unwrap();
    s.history[0].delivered = true;
    save(dir.path(), &s).unwrap();
    // Unchanged records are not appended again.
    save(dir.path(), &s).unwrap();
    assert_eq!(lines(dir.path(), LISTENS), 2);
    assert_eq!(lines(dir.path(), HISTORY), 2);
    let reloaded = load(dir.path()).unwrap();
    assert_eq!(reloaded.listens.len(), 1);
    assert_eq!(reloaded.listens[0].listened_ms, 250000);
    assert!(reloaded.history[0].delivered);
    assert_eq!(snapshot(&reloaded), snapshot(&s));
}

#[test]
fn torn_trailing_line_is_ignored_and_compacted_by_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(3, 0, 0);
    save(dir.path(), &s).unwrap();
    let mut bytes = fs::read(dir.path().join(LISTENS)).unwrap();
    // A damaged middle line and a torn final append.
    bytes.extend_from_slice(b"{\"namespace\":garbled\n");
    let mut extra = Vec::new();
    line(&mut extra, &listen(7)).unwrap();
    bytes.extend_from_slice(&extra[..extra.len() / 2]);
    fs::write(dir.path().join(LISTENS), &bytes).unwrap();
    let loaded = load(dir.path()).unwrap();
    assert_eq!(snapshot(&loaded), snapshot(&s));
    assert!(needs_maintenance(&loaded));
    // The next write rewrites instead of gluing a record onto the torn line.
    let mut next = loaded.clone();
    next.listens.push_back(listen(8));
    save_journals(dir.path(), &next).unwrap();
    assert_eq!(lines(dir.path(), LISTENS), 4);
    s.listens.push_back(listen(8));
    let reloaded = load(dir.path()).unwrap();
    assert_eq!(snapshot(&reloaded), snapshot(&s));
    assert!(!needs_maintenance(&reloaded));
}

#[test]
fn cap_evictions_append_and_replay_like_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(MAX_LISTENS, MAX_HISTORY, 0);
    save(dir.path(), &s).unwrap();
    // Exactly what features::record_play does at the caps.
    s.listens.pop_front();
    s.listens.push_back(listen(MAX_LISTENS));
    s.history.retain(|e| !e.delivered);
    s.history.push(play(MAX_HISTORY, false));
    let before = (ino(dir.path(), LISTENS), ino(dir.path(), HISTORY));
    save_journals(dir.path(), &s).unwrap();
    assert_eq!(
        before,
        (ino(dir.path(), LISTENS), ino(dir.path(), HISTORY)),
        "evictions are replayed, not compacted"
    );
    assert_eq!(lines(dir.path(), LISTENS), MAX_LISTENS + 1);
    assert_eq!(lines(dir.path(), HISTORY), MAX_HISTORY + 1);
    let reloaded = load(dir.path()).unwrap();
    assert_eq!(reloaded.listens.len(), MAX_LISTENS);
    assert_eq!(reloaded.history.len(), MAX_HISTORY / 2 + 1);
    assert_eq!(snapshot(&reloaded), snapshot(&s));
}

#[test]
fn diverging_memory_is_compacted_instead_of_appended() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(5, 3, 0);
    save(dir.path(), &s).unwrap();
    s.listens.remove(2);
    s.history.remove(1);
    save(dir.path(), &s).unwrap();
    assert_eq!(lines(dir.path(), LISTENS), 4);
    assert_eq!(lines(dir.path(), HISTORY), 2);
    assert_eq!(snapshot(&load(dir.path()).unwrap()), snapshot(&s));
}

#[test]
fn log_is_compacted_beyond_twice_its_cap() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(0, 0, 0);
    s.history = (0..10).map(|i| play(i, false)).collect();
    save(dir.path(), &s).unwrap();
    // 2 * cap lines of updates of the same ten plays.
    let mut bytes = fs::read(dir.path().join(HISTORY)).unwrap();
    for round in 0..(2 * MAX_HISTORY - 10) {
        line(&mut bytes, &play(round % 10, round % 2 == 0)).unwrap();
    }
    fs::write(dir.path().join(HISTORY), &bytes).unwrap();
    let mut loaded = load(dir.path()).unwrap();
    assert_eq!(lines(dir.path(), HISTORY), 2 * MAX_HISTORY);
    assert_eq!(loaded.history.len(), 10);
    assert!(!needs_maintenance(&loaded), "exactly 2 * cap still appends");
    let inode = ino(dir.path(), HISTORY);
    loaded.history[0].delivered = !loaded.history[0].delivered;
    save_journals(dir.path(), &loaded).unwrap();
    assert_ne!(inode, ino(dir.path(), HISTORY), "rewritten atomically");
    assert_eq!(lines(dir.path(), HISTORY), 10);
    assert_eq!(snapshot(&load(dir.path()).unwrap()), snapshot(&loaded));
    // Loading an oversized log asks the owner to compact.
    let mut bytes = fs::read(dir.path().join(HISTORY)).unwrap();
    for round in 0..(2 * MAX_HISTORY) {
        line(&mut bytes, &play(round % 10, true)).unwrap();
    }
    fs::write(dir.path().join(HISTORY), &bytes).unwrap();
    assert!(needs_maintenance(&load(dir.path()).unwrap()));
}

#[test]
fn position_checkpoint_rewrites_neither_queue_nor_logs() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(20, 20, 50);
    save(dir.path(), &s).unwrap();
    let files = [QUEUE, LISTENS, HISTORY].map(|f| ino(dir.path(), f));
    let session = ino(dir.path(), SESSION);
    save(dir.path(), &s).unwrap();
    assert_eq!(
        session,
        ino(dir.path(), SESSION),
        "identical session skipped"
    );
    s.playback.position += 1000;
    s.playback.listened += 1000;
    save(dir.path(), &s).unwrap();
    assert_ne!(session, ino(dir.path(), SESSION));
    assert_eq!(files, [QUEUE, LISTENS, HISTORY].map(|f| ino(dir.path(), f)));
    // A loaded lineage also knows the queue is already durable.
    let mut loaded = load(dir.path()).unwrap();
    loaded.playback.position += 1000;
    save(dir.path(), &loaded).unwrap();
    assert_eq!(files, [QUEUE, LISTENS, HISTORY].map(|f| ino(dir.path(), f)));
    loaded.playback.queue.select(1).unwrap();
    save(dir.path(), &loaded).unwrap();
    assert_ne!(files[0], ino(dir.path(), QUEUE));
    assert_eq!(snapshot(&load(dir.path()).unwrap()), snapshot(&loaded));
}

#[test]
fn queue_newer_than_session_drops_the_foreign_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let s = synthetic(0, 0, 3);
    save(dir.path(), &s).unwrap();
    let old_session = fs::read(dir.path().join(SESSION)).unwrap();
    let mut next = s.clone();
    next.playback
        .queue
        .replace(vec![item(7), item(8)], 1)
        .unwrap();
    save(dir.path(), &next).unwrap();
    // Crash after queue.json was replaced but before session.json.
    fs::write(dir.path().join(SESSION), old_session).unwrap();
    let loaded = load(dir.path()).unwrap();
    assert_eq!(loaded.playback.queue.items().len(), 2);
    assert_eq!(loaded.playback.position, 0);
    assert_eq!(loaded.playback.listened, 0);
    assert!(loaded.playback.occurrence.is_empty());
    assert_eq!(loaded.playback.generation, 7);
}

#[test]
fn corrupt_queue_is_preserved_and_loads_empty() {
    let dir = tempfile::tempdir().unwrap();
    save(dir.path(), &synthetic(1, 1, 3)).unwrap();
    fs::write(dir.path().join(QUEUE), b"{broken").unwrap();
    let loaded = load(dir.path()).unwrap();
    assert!(loaded.playback.queue.items().is_empty());
    assert_eq!(loaded.playback.position, 0);
    assert_eq!(loaded.listens.len(), 1);
    assert!(names(dir.path())
        .iter()
        .any(|n| n.starts_with("queue.json.corrupt-")));
    assert!(!dir.path().join(QUEUE).exists());
}

#[test]
fn fresh_settings_clear_every_file() {
    let dir = tempfile::tempdir().unwrap();
    let s = synthetic(4, 4, 4);
    save(dir.path(), &s).unwrap();
    // Logout replaces settings with a fresh lineage.
    let logged_out = Settings {
        client_identifier: s.client_identifier.clone(),
        ..Default::default()
    };
    save(dir.path(), &logged_out).unwrap();
    assert_eq!(names(dir.path()), [SESSION]);
    let loaded = load(dir.path()).unwrap();
    assert_eq!(snapshot(&loaded), snapshot(&logged_out));
    assert!(!needs_maintenance(&loaded));
}

#[test]
fn inspection_never_migrates_or_compacts() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(SESSION), LEGACY).unwrap();
    fs::write(dir.path().join(HISTORY), b"{\"torn").unwrap();
    let before: Vec<_> = names(dir.path())
        .into_iter()
        .map(|n| (fs::read(dir.path().join(&n)).unwrap(), n))
        .collect();
    drop(crate::Core::inspect(dir.path().into()).unwrap());
    for (bytes, name) in before {
        assert_eq!(bytes, fs::read(dir.path().join(&name)).unwrap(), "{name}");
    }
    assert!(!dir.path().join(LISTENS).exists());
    assert!(!dir.path().join(QUEUE).exists());
    // The owner migrates and compacts at startup.
    drop(crate::Core::new(dir.path().into()).unwrap());
    assert!(dir.path().join(LISTENS).exists());
    assert_eq!(lines(dir.path(), HISTORY), 2);
    let session = fs::read_to_string(dir.path().join(SESSION)).unwrap();
    assert!(!session.contains("\"listens\""));
}

/// Bytes written and wall time per playback checkpoint (PlaybackEvent +
/// SavePlayback) at the persistence caps. Run with
/// `cargo test --release --lib store::tests::bench -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_checkpoint() {
    for queue in [1000, 10000] {
        let mut s = synthetic(MAX_LISTENS, MAX_HISTORY, queue);
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        save(dir.path(), &s).unwrap();
        let size = |dir: &Path| {
            [SESSION, QUEUE, LISTENS, HISTORY]
                .iter()
                .map(|f| fs::metadata(dir.join(f)).map(|m| m.len()).unwrap_or(0))
                .sum::<u64>()
        };
        let n = 20;
        let mut written = 0;
        let started = std::time::Instant::now();
        for _ in 0..n {
            let before = size(dir.path());
            let session = fs::metadata(dir.path().join(SESSION)).unwrap().len();
            s.playback.position += 1000;
            s.playback.listened += 1000;
            s.listens.back_mut().unwrap().listened_ms += 1;
            save_journals(dir.path(), &s).unwrap(); // PlaybackEvent
            save(dir.path(), &s).unwrap(); // SavePlayback
                                           // Appended bytes plus the rewritten session.json.
            written += size(dir.path()) - before + session;
        }
        let elapsed = started.elapsed() / n;
        eprintln!(
            "SPLIT queue={queue}: session.json={} B, queue.json={} B, listens.jsonl={} B, \
             history.jsonl={} B; per checkpoint ~{} B written, {elapsed:?}",
            fs::metadata(dir.path().join(SESSION)).unwrap().len(),
            fs::metadata(dir.path().join(QUEUE)).unwrap().len(),
            fs::metadata(dir.path().join(LISTENS)).unwrap().len(),
            fs::metadata(dir.path().join(HISTORY)).unwrap().len(),
            written / u64::from(n),
        );
        let started = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(queue_state(&s.playback).unwrap());
        }
        eprintln!(
            "SPLIT queue={queue}: queue change check {:?}",
            started.elapsed() / n
        );
        let started = std::time::Instant::now();
        let loaded = load(dir.path()).unwrap();
        eprintln!(
            "SPLIT queue={queue}: load {:?} ({} listens)",
            started.elapsed(),
            loaded.listens.len()
        );
    }
}

#[test]
fn radio_progress_alone_rewrites_the_queue_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(0, 0, 3);
    save(dir.path(), &s).unwrap();
    let queue = ino(dir.path(), QUEUE);
    s.playback.radio = Some(crate::radio::Radio {
        kind: crate::radio::RadioKind::Track,
        title: "Radio".into(),
        source: crate::radio::Source::Station { queue_id: 7 },
    });
    save(dir.path(), &s).unwrap();
    assert_ne!(queue, ino(dir.path(), QUEUE));
    assert_eq!(snapshot(&load(dir.path()).unwrap()), snapshot(&s));
}

#[test]
fn reloaded_empty_queue_keeps_its_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = synthetic(0, 0, 0);
    s.playback.position = 5000;
    save(dir.path(), &s).unwrap();
    assert!(!dir.path().join(QUEUE).exists());
    let loaded = load(dir.path()).unwrap();
    assert_eq!(loaded.playback.position, 5000);
    let session = ino(dir.path(), SESSION);
    save(dir.path(), &loaded).unwrap();
    assert_eq!(session, ino(dir.path(), SESSION), "nothing changed");
}
