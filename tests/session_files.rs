//! Split session persistence through the public Core API.
use plexfreq_core::{Command, Core};
use std::os::unix::fs::MetadataExt;

fn lines(dir: &std::path::Path, file: &str) -> usize {
    std::fs::read(dir.join(file))
        .map(|b| b.iter().filter(|&&c| c == b'\n').count())
        .unwrap_or(0)
}

#[test]
fn playback_checkpoint_appends_one_listen_and_keeps_queue_file() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("session.json"),
        include_bytes!("fixtures/legacy-session.json"),
    )
    .unwrap();
    let mut core = Core::new(dir.path().into()).unwrap();
    // Startup migrated the old all-in-one session.
    assert_eq!(lines(dir.path(), "listens.jsonl"), 3);
    assert_eq!(lines(dir.path(), "history.jsonl"), 2);
    let queue = std::fs::metadata(dir.path().join("queue.json"))
        .unwrap()
        .ino();
    let status = core.execute(Command::Status).unwrap();
    assert_eq!(status["queue"]["items"].as_array().unwrap().len(), 3);
    assert_eq!(status["resumePosition"], 1234);
    let occurrence = status["playbackOccurrence"].as_str().unwrap().to_owned();
    // runtime::record issues exactly this pair at pause/stop/track change.
    core.execute(Command::PlaybackEvent {
        key: "100000".into(),
        occurrence: occurrence.clone(),
        listened: 230000,
        duration: 240000,
    })
    .unwrap();
    core.execute(Command::SavePlayback {
        key: "100000".into(),
        position: 5000,
        generation: status["playbackGeneration"].as_u64(),
    })
    .unwrap();
    assert_eq!(lines(dir.path(), "listens.jsonl"), 4);
    assert_eq!(lines(dir.path(), "history.jsonl"), 2);
    assert_eq!(
        queue,
        std::fs::metadata(dir.path().join("queue.json"))
            .unwrap()
            .ino()
    );
    drop(core);
    let mut core = Core::new(dir.path().into()).unwrap();
    let status = core.execute(Command::Status).unwrap();
    assert_eq!(status["resumePosition"], 5000);
    assert_eq!(status["resumeListened"], 230000);
    assert_eq!(status["playbackOccurrence"], occurrence.as_str());
    assert_eq!(status["queue"]["items"].as_array().unwrap().len(), 3);
    let insights = core
        .execute(Command::ListeningInsights { days: 0 })
        .unwrap();
    // The fixture's listens belong to another namespace; the new one counts.
    assert_eq!(insights["insights"]["plays"], 1);
    assert_eq!(insights["insights"]["listenedMs"], 230000);
    // Logout clears the queue and listening files.
    core.execute(Command::Logout).unwrap();
    for file in ["queue.json", "listens.jsonl", "history.jsonl"] {
        assert!(!dir.path().join(file).exists(), "{file}");
    }
}
