//! Offline snapshot pruning follows reads, not only writes.
use plexfreq_core::{
    model::{Container, Item},
    offline::Library,
};
use std::time::{Duration, SystemTime};

#[test]
fn reading_a_snapshot_refreshes_its_prune_age() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::new(dir.path().into(), true).unwrap();
    let container = Container {
        items: vec![Item {
            rating_key: "1".into(),
            kind: plexfreq_core::model::ItemKind::Track,
            ..Default::default()
        }],
        ..Default::default()
    };
    library.save("ns", "/library/metadata/1", &[], &container);
    let file = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with("ns-"))
        .unwrap()
        .path();
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(library.load("ns", "/library/metadata/1", &[]).is_some());
    assert!(std::fs::metadata(&file).unwrap().modified().unwrap() > old);
}

#[test]
fn offline_items_ignore_lyrics_files() {
    let dir = tempfile::tempdir().unwrap();
    let library = Library::new(dir.path().into(), true).unwrap();
    library.save_lyrics(
        "ns",
        "5",
        &[plexfreq_core::lyrics::Line {
            time: Some(0),
            text: "only".into(),
        }],
    );
    assert!(library.items("ns").is_empty());
}
