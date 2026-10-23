//! A move onto a cached successor leaves the radio refill to the background:
//! the refill is owed, fetched off the worker and applied only to the state
//! it was fetched for.
use crate::{
    plex::Probe,
    radio::{Radio, RadioKind, Source},
    radio_health_tests::{server, station, station_json},
    Command, Core,
};

fn pending(core: &Core) -> u8 {
    core.radio.as_ref().unwrap().refill.pending
}

fn keys(queue: &serde_json::Value) -> Vec<String> {
    queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["ratingKey"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_cached_successor_is_played_without_waiting_for_the_refill() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2]);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "2", "{plan}");
    assert!(server.requests("/playQueues/50").is_empty());
    assert_eq!(pending(&core), 1);
    // The owed refill, fetched as the runtime does, from the new position.
    let job = core.refill_job().expect("an owed refill");
    let update = core.refill_landed(job.fetch(), None).expect("applied");
    assert!(server.requests("/playQueues/50")[0].contains("center=102"));
    assert_eq!(keys(&update["queue"]), ["1", "2", "3", "4"]);
    assert_eq!(pending(&core), 0);
    assert!(core.refill_job().is_none());
    let saved = Core::inspect(dir.path().into())
        .unwrap()
        .execute(Command::Status)
        .unwrap();
    assert_eq!(keys(&saved["queue"]), ["1", "2", "3", "4"]);
}

#[test]
fn an_uncached_successor_or_the_end_still_refills_in_the_move() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[]);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(server.requests("/playQueues/50").len(), 1);
    assert_eq!(pending(&core), 0);
    // At the end, even with everything cached.
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[1, 2, 3]);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "4", "{plan}");
    assert_eq!(server.requests("/playQueues/50").len(), 2);
    assert_eq!(pending(&core), 0);
}

#[test]
fn a_synchronous_refill_pays_an_owed_one() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2]);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(pending(&core), 1);
    // Item 3 is not cached: this move refills itself and pays the debt.
    core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(pending(&core), 0);
}

#[test]
fn a_refill_for_an_older_state_is_dropped_and_fetched_again() {
    let server = server(|_| ("200 OK", station_json(&[5])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 4, 0, &[2, 3]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let stale = core.refill_job().unwrap().fetch();
    // Another move while it was in flight: the result is for an old window.
    core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(pending(&core), 2);
    assert!(core.refill_landed(stale, None).is_none());
    assert_eq!(pending(&core), 2, "kept for a new fetch");
    assert_eq!(core.queue.items().len(), 4);
    let fetched = core.refill_job().unwrap().fetch();
    let update = core.refill_landed(fetched, None).unwrap();
    assert_eq!(keys(&update["queue"]), ["1", "2", "3", "4", "5"]);
    assert_eq!(pending(&core), 1);
}

#[test]
fn a_restarted_radio_with_the_same_station_drops_the_old_refill() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let fetched = core.refill_job().unwrap().fetch();
    core.radio = Some(Radio::new(
        RadioKind::Artist,
        "Fixture".into(),
        Source::Station { queue_id: 50 },
    ));
    assert!(core.refill_landed(fetched, None).is_none());
    assert_eq!(core.queue.items().len(), 3);
}

#[test]
fn a_failed_refill_is_dropped_and_keeps_the_history() {
    let server = server(|_| ("500 Internal Server Error", String::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let fetched = core.refill_job().unwrap().fetch();
    assert!(core.refill_landed(fetched, None).is_none());
    assert_eq!(pending(&core), 0);
    assert!(core.refill_job().is_none());
    assert_eq!(core.queue.items().len(), 3);
}

#[test]
fn the_prepared_successor_keeps_a_refill_that_landed_before_its_transition() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2, 3]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let mut prepared = core.prepare_next().unwrap().expect("a successor");
    assert_eq!(prepared.key(), Some("3"));
    assert_eq!(prepared.radio.as_ref().unwrap().refill.pending, 2);
    let job = core.refill_job().unwrap();
    core.refill_landed(job.fetch(), Some(&mut prepared))
        .expect("applied");
    assert_eq!(keys(&prepared.plan["queue"]), ["1", "2", "3", "4"]);
    let plan = core.commit_prepared(prepared).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "3");
    assert_eq!(keys(&plan["queue"]), ["1", "2", "3", "4"]);
    // The speculative move's own refill is still owed.
    assert_eq!(pending(&core), 1);
}

#[test]
fn a_gone_successor_is_not_waited_past() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[3]);
    // Item 2 is gone and not cached: the move steps over it and refills.
    core.record_probe("2", Probe::Gone);
    core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(server.requests("/playQueues/50").len(), 1);
}

#[test]
fn a_prepared_successor_of_another_radio_state_is_left_to_its_commit() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2, 3]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let mut prepared = core.prepare_next().unwrap().unwrap();
    // As if the speculation had refilled itself.
    prepared.radio.as_mut().unwrap().refill.version += 1;
    let fetched = core.refill_job().unwrap().fetch();
    core.refill_landed(fetched, Some(&mut prepared)).unwrap();
    assert_eq!(keys(&prepared.plan["queue"]), ["1", "2", "3"]);
    assert_eq!(prepared.radio.as_ref().unwrap().refill.pending, 2);
}

#[test]
fn a_failed_refill_also_clears_the_prepared_debt() {
    let server = server(|_| ("500 Internal Server Error", String::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2, 3]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let mut prepared = core.prepare_next().unwrap().unwrap();
    let fetched = core.refill_job().unwrap().fetch();
    assert!(core.refill_landed(fetched, Some(&mut prepared)).is_none());
    assert_eq!(prepared.radio.as_ref().unwrap().refill.pending, 0);
    core.commit_prepared(prepared).unwrap();
    assert!(core.refill_job().is_none());
}

#[test]
fn a_radio_saved_without_refill_state_gets_its_own_instance() {
    let saved =
        serde_json::json!({"kind":"artist","title":"Fixture","source":{"Station":{"queue_id":50}}});
    let first: Radio = serde_json::from_value(saved.clone()).unwrap();
    let second: Radio = serde_json::from_value(saved).unwrap();
    assert_eq!(first.refill.pending, 0);
    assert_ne!(first.refill.instance, second.refill.instance);
    let mut refill = first.refill;
    for _ in 0..20 {
        refill.owe();
    }
    assert_eq!(refill.pending, crate::radio::Refill::MAX_PENDING);
}

#[test]
fn an_abandoned_refill_stays_abandoned_after_the_prepared_commit() {
    let server = server(|_| ("200 OK", station_json(&[4])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 0, &[2, 3]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let mut prepared = core.prepare_next().unwrap().unwrap();
    core.refill_abandon(Some(&mut prepared));
    core.commit_prepared(prepared).unwrap();
    assert!(core.refill_job().is_none());
}

#[test]
fn refresh_warning_is_observational_and_survives_discarded_preparation() {
    let server = server(|_| ("500 Internal Server Error", String::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[1, 2, 3]);
    let prepared = core.prepare_next().unwrap().unwrap();
    assert_eq!(core.current_key(), Some("3"));
    assert_eq!(prepared.key(), Some("1"));
    assert!(core.take_radio_warning(), "the real refresh request failed");
    assert!(!core.take_radio_warning());
    drop(prepared);
    let plan = core.execute(Command::Next { automatic: false }).unwrap();
    assert_eq!(plan["track"]["ratingKey"], "1");
    assert_eq!(core.queue.items().len(), 3);
    assert!(core.take_radio_warning());
    assert!(plan.get("radio_warning").is_none());
}

#[test]
fn accepted_failed_refill_warns_but_stale_and_explicit_offline_do_not() {
    let server = server(|_| ("500 Internal Server Error", String::new()));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 4, 0, &[1, 2, 3, 4]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let stale = core.refill_job().unwrap().fetch();
    core.execute(Command::Next { automatic: false }).unwrap();
    core.refill_landed(stale, None);
    assert!(!core.take_radio_warning());
    let failed = core.refill_job().unwrap().fetch();
    core.refill_landed(failed, None);
    assert!(core.take_radio_warning());
    core.execute(Command::Next { automatic: false }).unwrap();
    let failed = core.refill_job().unwrap().fetch();
    core.offline_mode = true;
    core.refill_landed(failed, None);
    assert!(!core.take_radio_warning());
    core.execute(Command::Next { automatic: false }).unwrap();
    assert!(!core.take_radio_warning());
    core.offline_mode = false;
    core.warn_radio();
    core.offline_mode = true;
    assert!(
        !core.take_radio_warning(),
        "offline discards pending warnings too"
    );
}

#[test]
fn empty_and_duplicate_station_results_warn_without_changing_success() {
    for additions in [vec![], vec![1, 2, 3], vec![4]] {
        let fresh = additions == [4];
        let server = server(move |_| ("200 OK", station_json(&additions)));
        let dir = tempfile::tempdir().unwrap();
        let mut core = station(dir.path(), &server, 3, 0, &[]);
        let plan = core.execute(Command::Next { automatic: false }).unwrap();
        assert_eq!(plan["track"]["ratingKey"], "2");
        assert_eq!(core.queue.items().len(), if fresh { 4 } else { 3 });
        assert_eq!(pending(&core), 0);
        assert_eq!(core.take_radio_warning(), !fresh);
    }
}

#[test]
fn lost_refill_worker_cannot_warn_or_abandon_a_new_state() {
    let server = server(|_| ("200 OK", station_json(&[5])));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 4, 0, &[2, 3, 4]);
    core.execute(Command::Next { automatic: false }).unwrap();
    let old = core.refill_job().unwrap().request.identity;
    core.execute(Command::Next { automatic: false }).unwrap();
    core.refill_worker_failed(&old, None, false);
    assert_eq!(pending(&core), 2);
    assert!(!core.take_radio_warning());
    let current = core.refill_identity().unwrap();
    core.refill_worker_failed(&current, None, true);
    assert_eq!(pending(&core), 2);
    assert!(
        !core.take_radio_warning(),
        "poll already published the successor"
    );
    let mut rebuilt = Core::inspect(dir.path().into()).unwrap();
    rebuilt.refill_worker_failed(&current, None, false);
    assert_eq!(pending(&rebuilt), 2);
    assert!(
        !rebuilt.take_radio_warning(),
        "Core rebuild invalidates workers"
    );
    core.refill_worker_failed(&current, None, false);
    assert_eq!(pending(&core), 0);
    assert!(core.take_radio_warning());
}

#[test]
fn known_disconnection_warns_but_deliberate_offline_does_not() {
    let server = server(|_| panic!("disconnected radio must not request the server"));
    let dir = tempfile::tempdir().unwrap();
    let mut core = station(dir.path(), &server, 3, 2, &[1, 2, 3]);
    core.network_known = true;
    core.network_online = false;
    core.execute(Command::Next { automatic: false }).unwrap();
    assert!(core.take_radio_warning());
    core.offline_mode = true;
    core.execute(Command::Next { automatic: false }).unwrap();
    assert!(!core.take_radio_warning());
}

#[test]
fn sonic_cached_response_and_nested_album_fallback_warn_only_during_refill() {
    use crate::{
        model::{Container, ItemKind},
        radio_health_tests::item,
        Error,
    };
    for album in [false, true] {
        let server = server(|_| ("500 Internal Server Error", String::new()));
        let dir = tempfile::tempdir().unwrap();
        let mut core = station(dir.path(), &server, 3, 0, &[]);
        let mut seed = item(1);
        let mut next = item(4);
        if album {
            seed.kind = ItemKind::Album;
            next.kind = ItemKind::Album;
        }
        core.radio = Some(Radio::new(
            if album {
                RadioKind::Album
            } else {
                RadioKind::Track
            },
            "Fixture".into(),
            Source::Sonic {
                seed: Box::new(seed),
                recent: vec![],
            },
        ));
        let request = core.refill_request().unwrap();
        let neighbors = Container {
            items: vec![next],
            ..Default::default()
        };
        core.library.save(
            &core.cache_namespace(),
            &request.path,
            &request.params,
            &neighbors,
        );
        if album {
            // The nearest request succeeds, but expansion fails and uses cache.
            let children = Container {
                items: vec![item(4)],
                ..Default::default()
            };
            core.library.save(
                &core.cache_namespace(),
                "/library/metadata/4/children",
                &[
                    ("X-Plex-Container-Start", "0".into()),
                    ("X-Plex-Container-Size", "100".into()),
                ],
                &children,
            );
        }
        core.cached_response(&request.path, &request.params, Err(Error::Network))
            .unwrap();
        assert!(
            !core.take_radio_warning(),
            "ordinary browsing fallback is not radio failure"
        );
        let fetched = if album {
            Ok(neighbors)
        } else {
            Err(Error::Network)
        };
        let added = core.refill_apply(&request, fetched).unwrap();
        assert!(!added.is_empty());
        assert!(core.take_radio_warning());
    }
}
