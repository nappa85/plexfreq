//! Production review regressions asserting corrected queue/parser/cache behavior.

use plexfreq_core::{
    cache::{Cache, Config},
    model::Item,
    queue::{Queue, Repeat},
};
use serde_json::json;

fn track(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: plexfreq_core::model::ItemKind::Track,
        title: format!("Track {id}"),
        ..Default::default()
    }
}

// Repeat::All wraps forward via advance(), so previous() at the first slot
// must wrap to the last slot instead of sticking at the first track.
#[test]
fn queue_previous_wraps_on_repeat_all() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.set_repeat(Repeat::All);
    assert_eq!(q.track().unwrap().rating_key, "0");
    let wrapped = q.previous().unwrap().rating_key.clone();
    assert_eq!(
        wrapped, "2",
        "repeat-all previous() at first track must wrap to last, stayed at {wrapped}"
    );
}

// A no-op reorder must not change playback policy: moving a row onto itself
// must preserve the shuffled flag and the playback order.
#[test]
fn queue_move_noop_preserves_shuffle() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.shuffle(true);
    assert!(q.shuffled());
    let before = q.upcoming(10);
    q.move_item(1, 1).unwrap();
    assert!(
        q.shuffled(),
        "no-op move_item(1,1) exited shuffle; shuffled flag lost"
    );
    assert_eq!(
        q.upcoming(10)
            .iter()
            .map(|i| &i.rating_key)
            .collect::<Vec<_>>(),
        before.iter().map(|i| &i.rating_key).collect::<Vec<_>>(),
        "no-op move changed playback order"
    );
}

// presentation() linearizes order into identity, so it must not keep
// shuffled:true (a DTO claiming to be shuffled while order is identity).
#[test]
fn queue_presentation_does_not_claim_shuffled_with_identity_order() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1"), track("2")], 0)
        .unwrap();
    q.shuffle(true);
    let shown = q.presentation();
    assert!(
        !shown.shuffled(),
        "presentation linearizes order but keeps shuffled:true"
    );
}

// Plain lyric text starting with '{' is content, not broken JSON. decode()
// falls back to parse() for '[' and '"' but returns Err for '{'.
#[test]
fn lyrics_brace_plain_text_is_content_not_error() {
    let lines = plexfreq_core::lyrics::decode("{hello}").unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].text, "{hello}");
}

// Timed PMS JSON with float start values must produce timestamps.
// as_u64() drops 1500.5/2500.75 to None, desyncing timed display.
#[test]
fn lyrics_float_start_produces_timestamps() {
    let payload = json!({"MediaContainer":{"Lyrics":[{"timed":true,
        "Line":[{"Span":[{"text":"hi","start":1500.5}],"start":2500.75}]}]}})
    .to_string();
    let lines = plexfreq_core::lyrics::decode(&payload).unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].time, Some(2501));
    for (start, expected) in [
        (json!(1500.5), Some(1501)),
        (json!(-0.5), None),
        (json!(18446744073709551616.0), None),
        (json!(null), None),
    ] {
        let payload = json!({"MediaContainer":{"Lyrics":[{"timed":true,
            "Line":[{"Span":[{"text":"hi","start":start}]}]}]}});
        let lines = plexfreq_core::lyrics::decode(&payload.to_string()).unwrap();
        assert_eq!(lines[0].time, expected);
    }
}

// PIN integers accept numeric strings, but required_u64 rejects padded
// values (" 42") while every lenient parser trims first.
#[test]
fn pin_whitespace_padded_integers_parse() {
    let payload = json!({"id":" 42 ","code":"fixture","expiresIn":" 3600 ","authToken":null});
    let pin: plexfreq_core::model::Pin = serde_json::from_value(payload)
        .expect("whitespace-padded PIN integers must parse like lenient fields");
    assert_eq!(pin.id, 42);
    assert_eq!(pin.expires_in, 3600);
}

// PMS booleans vary: radio_flag must accept float 1.0 and padded " 1"
// like the lenient numeric parsers accept floats/whitespace.
#[test]
fn model_radio_flag_accepts_float_and_padded_string() {
    for smart in [json!(1.0), json!(" 1 "), json!("TRUE")] {
        let payload = json!({"MediaContainer":{"Metadata":[
            {"ratingKey":"1","key":"/a","title":"T","type":"track","smart":smart}]}});
        let envelope: plexfreq_core::model::Envelope = serde_json::from_value(payload).unwrap();
        assert!(
            envelope.container.items[0].smart,
            "radio_flag rejected {smart:?}; must be true"
        );
    }
}

// Optional gain accepts numeric strings but does not trim, while
// lenient_f64 trims. Padded gain must parse instead of being dropped.
#[test]
fn model_optional_gain_trims_whitespace() {
    let payload = json!({"MediaContainer":{"Metadata":[
        {"ratingKey":"1","key":"/a","title":"T","type":"track",
         "Media":[{"Part":[{"key":"/p","Stream":[
            {"key":"/s","streamType":4,"gain":" -6.0 "}]}]}]}]}});
    let envelope: plexfreq_core::model::Envelope = serde_json::from_value(payload).unwrap();
    let gain = envelope.container.items[0].media[0].parts[0].streams[0].gain;
    assert!(
        gain.is_some_and(|g| (g + 6.0).abs() < 0.001),
        "padded gain string dropped; got {gain:?}"
    );
}

// station_uri() must require a server-relative key with a leading '/'.
// station_key() validates via advertised_path/server_path, but station_uri()
// concatenates unchecked, so "abc" becomes "...libraryabc".
#[test]
fn radio_station_uri_rejects_bare_key() {
    assert!(
        plexfreq_core::radio::station_uri("server-123", "abc").is_err(),
        "bare key without leading '/' was accepted as a station URI"
    );
    assert!(
        plexfreq_core::radio::station_uri("server-123", "").is_err(),
        "empty key was accepted as a station URI"
    );
    for key in [
        "//external.invalid/a",
        "/a/../b",
        "/a%2fb",
        "/a?X-Plex-Token=fixture",
    ] {
        assert!(plexfreq_core::radio::station_uri("server-123", key).is_err());
    }
}

// One malformed index entry must not brick the whole audio cache.
// Malformed records are skipped without losing usable entries.
#[test]
fn cache_single_bad_entry_does_not_brick_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("good.wav"), b"1234567").unwrap();
    let ns = plexfreq_core::cache::namespace("http://fixture.invalid/", "fixture-token");
    let item = track("1");
    let good = json!({
        "namespace": ns,
        "item": item,
        "file": "good.wav",
        "bytes": 7,
        "used": 1
    });
    let bad = json!({"namespace": "ns", "item": {}, "file": "bad.wav"});
    use sha2::{Digest, Sha256};
    let good_key = format!(
        "{:x}",
        Sha256::digest(format!("{ns}\0{}\0", item.rating_key))
    );
    std::fs::write(
        dir.path().join("index.json"),
        json!({"entries": {good_key: good, "bad": bad,
            "wrongType":{"namespace":"ns","item":{},"file":"bad.wav","bytes":"bad","used":1}}})
        .to_string(),
    )
    .unwrap();
    let cache = Cache::new(dir.path().into(), Config::default(), "fixture")
        .expect("one bad index entry must not fail cache open");
    assert_eq!(
        cache.lookup(&ns, &item, false).unwrap().0,
        dir.path().join("good.wav")
    );
    cache.cancel();
    drop(cache);
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("index.json")).unwrap()).unwrap();
    assert_eq!(manifest["entries"].as_object().unwrap().len(), 1);
    let reopened = Cache::new(dir.path().into(), Config::default(), "fixture").unwrap();
    assert!(reopened.lookup(&ns, &item, false).is_some());
}

#[test]
fn core_noop_move_preserves_shuffle_policy_and_display_order() {
    use plexfreq_core::{Command, Core};
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Enqueue {
        items: vec![track("0"), track("1"), track("2")],
        next: false,
    })
    .unwrap();
    core.execute(Command::Shuffle { enabled: true }).unwrap();
    let before = core.execute(Command::Status).unwrap();
    assert_eq!(before["queue"]["shuffled"], true);
    let after = core.execute(Command::QueueMove { from: 1, to: 1 }).unwrap();
    assert_eq!(after["queue"], before["queue"]);
    assert!(core.execute(Command::QueueMove { from: 3, to: 3 }).is_err());
}
