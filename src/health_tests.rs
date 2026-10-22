//! Core navigation over items marked gone (stale-queue stage 1).
use crate::{
    model::{Item, ItemKind, Media, Part},
    plex::Probe,
    queue::Repeat,
    Command, Core, Skip, GONE_TRACK,
};

fn track(key: usize, media: bool) -> Item {
    Item {
        rating_key: key.to_string(),
        kind: ItemKind::Track,
        title: format!("Track {key}"),
        duration: 1000,
        media: if media {
            vec![Media {
                parts: vec![Part {
                    key: format!("/library/parts/{key}/file.wav"),
                    ..Default::default()
                }],
                ..Default::default()
            }]
        } else {
            Vec::new()
        },
        ..Default::default()
    }
}

/// A Core with a queue of `count` tracks, current at `start`, no network
/// and no cache-ahead downloads.
fn core(dir: &std::path::Path, count: usize, start: usize) -> Core {
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        server_url: "http://127.0.0.1:9/".into(),
        server_token: "fixture-token".into(),
        autoplay: false,
        cache: crate::cache::Config {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    settings
        .playback
        .queue
        .replace((1..=count).map(|key| track(key, true)).collect(), start)
        .unwrap();
    crate::store::save(dir, &settings).unwrap();
    Core::new(dir.into()).unwrap()
}

fn current(core: &mut Core) -> Option<String> {
    core.current_key().map(str::to_owned)
}

#[test]
fn next_steps_over_gone_items() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 4, 0);
    core.record_probe("2", Probe::Gone);
    core.record_probe("3", Probe::Gone);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(current(&mut core).as_deref(), Some("4"));
    assert!(!core.take_gone_notice());
    // Automatic advance does the same.
    drop(core);
    let mut core = self::core(dir.path(), 4, 0);
    core.record_probe("2", Probe::Gone);
    core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(current(&mut core).as_deref(), Some("3"));
}

#[test]
fn a_gone_tail_ends_playback_with_a_notice_and_is_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 0);
    core.record_probe("2", Probe::Gone);
    core.record_probe("3", Probe::Gone);
    let plan = core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(plan["stream"], "", "{plan}");
    assert!(core.take_gone_notice());
    assert!(!core.take_gone_notice(), "the notice is taken once");
    assert_eq!(current(&mut core), None);
    let reloaded = Core::inspect(dir.path().into())
        .unwrap()
        .execute(Command::Status)
        .unwrap();
    assert!(reloaded["track"].is_null(), "{reloaded}");
    // Previous from the natural end returns past the gone tail.
    core.execute(Command::Previous).unwrap();
    assert_eq!(current(&mut core).as_deref(), Some("1"));
    // Also after a restart, when the (unpersisted) marks are gone.
    core.record_probe("2", Probe::Gone);
    core.record_probe("3", Probe::Gone);
    core.execute(Command::Next { automatic: true }).unwrap();
    assert!(core.take_gone_notice());
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    core.execute(Command::Previous).unwrap();
    assert_eq!(current(&mut core).as_deref(), Some("1"));
}

#[test]
fn a_failed_command_leaves_no_gone_notice() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 1);
    core.health.set_notice();
    // An invalid replacement fails after the notice was raised.
    assert!(core
        .execute(Command::Play {
            items: Vec::new(),
            index: 0
        })
        .is_err());
    assert!(!core.take_gone_notice());
}

#[test]
fn previous_steps_back_over_gone_items_and_fails_at_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 4, 3);
    core.record_probe("3", Probe::Gone);
    core.execute(Command::Previous).unwrap();
    assert_eq!(current(&mut core).as_deref(), Some("2"));
    drop(core);
    let mut core = self::core(dir.path(), 3, 1);
    core.record_probe("1", Probe::Gone);
    let error = core.execute(Command::Previous).unwrap_err();
    assert_eq!(error.to_string(), GONE_TRACK);
    // The queue is restored.
    assert_eq!(current(&mut core).as_deref(), Some("2"));
}

#[test]
fn explicit_select_and_resume_retry_a_gone_item() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 0);
    core.record_probe("2", Probe::Gone);
    let plan = core.execute(Command::SelectTrack { index: 1 }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
    assert!(!core.is_gone("2"));
    core.record_probe("2", Probe::Gone);
    let plan = core.execute(Command::Resume).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
    // The fallback path refuses a gone item instead of trying a transcode.
    core.record_probe("2", Probe::Gone);
    assert_eq!(
        core.execute(Command::TranscodedPlayback)
            .map(|_| ())
            .unwrap_err()
            .to_string(),
        GONE_TRACK
    );
}

#[test]
fn removing_the_active_item_keeps_the_removal_before_a_gone_tail() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 1);
    core.record_probe("3", Probe::Gone);
    let plan = core.execute(Command::QueueRemove { index: 1 }).unwrap();
    assert_eq!(plan["stream"], "", "{plan}");
    assert!(core.take_gone_notice());
    assert_eq!(
        core.execute(Command::Status).unwrap()["queue"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // Removing the last item with nothing after it is a plain end.
    drop(core);
    let mut core = self::core(dir.path(), 2, 1);
    core.execute(Command::QueueRemove { index: 1 }).unwrap();
    assert!(!core.take_gone_notice());
}

#[test]
fn a_new_queue_clears_marks_and_a_failed_one_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 2, 0);
    core.record_probe("2", Probe::Gone);
    assert!(core
        .execute(Command::Play {
            items: Vec::new(),
            index: 0
        })
        .is_err());
    assert!(core.is_gone("2"), "a failed replacement restores the marks");
    let items = vec![track(2, true), track(5, true)];
    let plan = core.execute(Command::Play { items, index: 0 }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
    assert!(!core.is_gone("2"));
}

#[test]
fn repeat_all_over_an_all_gone_queue_terminates() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 0);
    core.execute(Command::Repeat { mode: Repeat::All }).unwrap();
    for key in ["1", "2", "3"] {
        core.record_probe(key, Probe::Gone);
    }
    let plan = core.execute(Command::Next { automatic: true }).unwrap();
    assert_eq!(plan["stream"], "", "{plan}");
    assert!(core.take_gone_notice());
}

#[test]
fn preparation_skips_gone_items_without_reporting() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 0);
    core.record_probe("2", Probe::Gone);
    let prepared = core.prepare_next().unwrap().expect("a successor");
    assert_eq!(prepared.key(), Some("3"));
    assert_eq!(current(&mut core).as_deref(), Some("1"));
    core.record_probe("3", Probe::Gone);
    assert!(core.prepare_next().unwrap().is_none());
    assert!(!core.take_gone_notice(), "speculation never reports");
}

#[test]
fn skip_planning_reports_where_an_automatic_skip_would_land() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 3, 0);
    assert_eq!(core.peek_skip(false), Skip::Ready);
    assert_eq!(core.peek_skip(true), Skip::Stuck);
    core.record_probe("2", Probe::Gone);
    core.record_probe("3", Probe::Gone);
    assert_eq!(core.peek_skip(false), Skip::End);
    // A target without file details would wait on the network.
    let mut settings = crate::store::load(dir.path()).unwrap();
    settings
        .playback
        .queue
        .replace(vec![track(1, true), track(2, false)], 0)
        .unwrap();
    drop(core);
    crate::store::save(dir.path(), &settings).unwrap();
    let core = Core::new(dir.path().into()).unwrap();
    assert_eq!(core.peek_skip(false), Skip::NeedsMetadata);
}

#[test]
fn unknown_and_auth_outcomes_only_cool_down() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 2, 0);
    assert!(core.may_probe("2"));
    core.record_probe("2", Probe::Unknown);
    assert!(!core.is_gone("2"));
    assert!(!core.may_probe("2"));
    core.record_probe("1", Probe::Auth);
    assert!(!core.is_gone("1"));
}

#[test]
fn the_gone_message_matches_its_translatable_literal() {
    let error = crate::Error::Input("This track is no longer on the server");
    assert_eq!(error.to_string(), GONE_TRACK);
}

/// The UI marks gone tracks from the cache status, which every cache-status
/// push carries.
#[test]
fn cache_status_lists_gone_tracks() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 4, 0);
    let status = core.execute(Command::CacheStatus).unwrap();
    assert_eq!(status["cache"]["goneKeys"], serde_json::json!([]));
    core.record_probe("3", Probe::Gone);
    core.record_probe("2", Probe::Gone);
    let status = core.execute(Command::CacheStatus).unwrap();
    assert_eq!(status["cache"]["goneKeys"], serde_json::json!(["2", "3"]));
}
