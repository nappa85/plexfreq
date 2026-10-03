use plexfreq_core::{model::Item, queue::Queue, Command, Core};
use serde_json::json;

fn track(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: "track".into(),
        duration: 120000,
        ..Default::default()
    }
}

#[test]
fn edits_preserve_current_occurrence_and_next_order_under_shuffle() {
    let mut queue = Queue::default();
    queue
        .replace(vec![track("1"), track("2"), track("3")], 1)
        .unwrap();
    queue.shuffle(true);
    queue.insert(vec![track("4"), track("5")], true).unwrap();
    assert_eq!(
        queue
            .upcoming(3)
            .iter()
            .map(|i| i.rating_key.as_str())
            .collect::<Vec<_>>(),
        vec!["2", "4", "5"]
    );
    let mut shown = queue.presentation();
    shown.move_item(1, 3).unwrap();
    assert_eq!(shown.track().unwrap().rating_key, "2");
    assert!(!shown.shuffled);
    shown.remove(1).unwrap();
    assert_eq!(shown.track().unwrap().rating_key, "2");
    shown.validate().unwrap();
    let restored: Queue = serde_json::from_value(serde_json::to_value(&queue).unwrap()).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.upcoming(5), queue.upcoming(5));
    let mut last = Queue::default();
    last.replace(vec![track("1"), track("2")], 1).unwrap();
    assert!(last.remove(1).unwrap());
    assert!(last.track().is_none());
}

#[test]
fn queue_and_resume_survive_restart_without_autoplay_or_authenticated_urls() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut core = Core::new(dir.path().into()).unwrap();
        core.execute(Command::Enqueue {
            items: vec![track("1"), track("2")],
            next: false,
        })
        .unwrap();
        // No stream plan is requested just by adding items.
        assert_eq!(
            core.execute(Command::Status).unwrap()["queue"]["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
    let mut core = Core::new(dir.path().into()).unwrap();
    let status = core.execute(Command::Status).unwrap();
    assert!(status.get("stream").is_none());
    assert_eq!(status["queue"]["items"][0]["ratingKey"], "1");
    core.execute(Command::QueueMove { from: 1, to: 0 }).unwrap();
    core.execute(Command::QueueRemove { index: 1 }).unwrap();
    assert_eq!(
        core.execute(Command::Status).unwrap()["queue"]["items"][0]["ratingKey"],
        "2"
    );
    let stored: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("session.json")).unwrap()).unwrap();
    assert!(stored["playback"]["queue"]["items"][0]
        .get("artwork")
        .is_none());
    assert_eq!(stored["playback"]["queue"]["current"], json!(null));
}
