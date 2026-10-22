//! Paging of the saved (offline) library browse.
use crate::{model::ItemKind, Command, Core};
use serde_json::{json, Value};

/// A Core whose offline library holds `artists` saved artists (titles with
/// ties broken by key) and nothing else.
fn core(dir: &std::path::Path, artists: usize) -> Core {
    crate::store::save(
        dir,
        &crate::store::Settings {
            client_identifier: "fixture-client".into(),
            server_url: "http://127.0.0.1:9/".into(),
            server_token: "fixture-token".into(),
            cache: crate::cache::Config {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .unwrap();
    let core = Core::new(dir.into()).unwrap();
    let items = (0..artists)
        .map(|i| crate::model::Item {
            rating_key: (1000 + i).to_string(),
            kind: ItemKind::Artist,
            // Titles descend while keys ascend, in mixed case; pairs share a
            // title (case aside) so the key must break the tie.
            title: if i % 2 == 0 {
                format!("artist {:03}", (artists - 1 - i) / 2)
            } else {
                format!("ARTIST {:03}", (artists - 1 - i) / 2)
            },
            ..Default::default()
        })
        .collect();
    let container = crate::model::Container {
        items,
        ..Default::default()
    };
    core.library.save(
        &core.cache_namespace(),
        "/library/sections/1/all",
        &[],
        &container,
    );
    core
}

fn page(core: &mut Core, start: usize) -> Value {
    core.execute(Command::OfflineBrowse {
        kind: ItemKind::Artist,
        start,
    })
    .unwrap()
}

fn keys(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["ratingKey"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn offline_browse_pages_in_a_stable_title_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 250);
    let first = page(&mut core, 0);
    assert_eq!(
        (first["start"].clone(), first["next"].clone()),
        (json!(0), json!(100))
    );
    assert_eq!(first["hasMore"], true);
    let second = page(&mut core, 100);
    let third = page(&mut core, 200);
    assert_eq!(third["next"], 250);
    assert_eq!(third["hasMore"], false);
    let all: Vec<_> = [&first, &second, &third]
        .into_iter()
        .flat_map(keys)
        .collect();
    assert_eq!(all.len(), 250);
    // Case-insensitive title order (the reverse of key order), ties by key,
    // no item twice across pages.
    let mut expected: Vec<(usize, usize)> = (0..250).map(|i| ((249 - i) / 2, 1000 + i)).collect();
    expected.sort();
    let expected: Vec<_> = expected
        .into_iter()
        .map(|(_, key)| key.to_string())
        .collect();
    assert_eq!(all, expected);
    // Past the end: an empty, final page.
    let past = page(&mut core, 400);
    assert!(keys(&past).is_empty());
    assert_eq!(past["hasMore"], false);
}

#[test]
fn offline_browse_has_no_phantom_page_at_exact_multiples_or_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = core(dir.path(), 100);
    let only = page(&mut core, 0);
    assert_eq!(keys(&only).len(), 100);
    assert_eq!(only["hasMore"], false);
    let dir = tempfile::tempdir().unwrap();
    let mut empty = self::core(dir.path(), 0);
    let none = page(&mut empty, 0);
    assert!(keys(&none).is_empty());
    assert_eq!(none["hasMore"], false);
    assert!(empty
        .execute(Command::OfflineBrowse {
            kind: ItemKind::Playlist,
            start: 0,
        })
        .is_err());
}

/// The UI sends `{op, kind}` first and adds `start` for later pages; the
/// runtime appends a page with a nonzero start.
#[test]
fn offline_browse_wire_requests_carry_the_page_start() {
    let first = Command::parse(&json!({"op":"offline_browse","kind":"artist"})).unwrap();
    assert_eq!(first.start(), 0);
    let later =
        Command::parse(&json!({"op":"offline_browse","kind":"artist","start":100})).unwrap();
    assert_eq!(later.start(), 100);
    assert!(later.class().listing);
}
