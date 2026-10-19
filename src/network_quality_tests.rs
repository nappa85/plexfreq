//! Streaming quality follows the cellular network fact (mobile streaming,
//! part A): Core-level effects that pure mapping tests cannot show.
use crate::{
    model::{Item, ItemKind, Media, Part},
    radio_health_tests::server,
    Command, Core,
};
use std::time::{Duration, Instant};

fn item(key: usize) -> Item {
    Item {
        rating_key: key.to_string(),
        kind: ItemKind::Track,
        title: format!("Track {key}"),
        duration: 1000,
        media: vec![Media {
            parts: vec![Part {
                key: format!("/library/parts/{key}/file.flac"),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[test]
fn a_cellular_change_alone_turns_capped_prefetch_off_and_on() {
    let server = server(|target| {
        if target.starts_with("/library/parts/") {
            ("200 OK", "audio".into())
        } else {
            ("200 OK", r#"{"MediaContainer":{"size":0}}"#.into())
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut settings = crate::store::Settings {
        client_identifier: "fixture-client".into(),
        server_url: server.origin.clone(),
        server_token: "fixture-token".into(),
        autoplay: false,
        cache: crate::cache::Config {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };
    // Downloads allowed off Wi-Fi, so only the quality cap decides.
    settings.wifi_only = false;
    settings.quality.wifi_kbps = 0;
    settings.quality.mobile_kbps = 160;
    settings.quality.download_kbps = 0;
    settings
        .playback
        .queue
        .replace((1..=3).map(item).collect(), 0)
        .unwrap();
    crate::store::save(dir.path(), &settings).unwrap();
    // Prefetch requests: original parts, or transcodes for unknown codecs.
    let media =
        || server.requests("/library/parts/").len() + server.requests("/music/:/transcode/").len();
    let mut core = Core::new(dir.path().into()).unwrap();
    let reply = core
        .execute(Command::NetworkState {
            wifi: false,
            online: true,
            cellular: true,
            live_hint: false,
        })
        .unwrap();
    assert_eq!(reply["networkCellular"], true, "{reply}");
    core.execute(Command::CacheConfig {
        enabled: true,
        limit_mb: 512,
        ahead: 5,
    })
    .unwrap();
    // Capped streaming on cellular: no original-file prefetch.
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(media(), 0, "{:?}", server.requests(""));
    // Ethernet: Wi-Fi and online unchanged, only the cellular fact flips.
    core.execute(Command::NetworkState {
        wifi: false,
        online: true,
        cellular: false,
        live_hint: false,
    })
    .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    while media() == 0 {
        assert!(Instant::now() < end, "prefetch never started");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_session_without_a_stored_mobile_choice_gets_320() {
    let legacy: crate::quality::Config =
        serde_json::from_str(r#"{"wifiKbps":0,"downloadKbps":0,"codecFallback":true}"#).unwrap();
    assert_eq!(legacy.mobile_kbps, 320);
    let chosen: crate::quality::Config = serde_json::from_str(r#"{"mobileKbps":0}"#).unwrap();
    assert_eq!(chosen.mobile_kbps, 0);
    assert_eq!(chosen.streaming(true), 0);
    assert_eq!(legacy.streaming(true), 320);
    assert_eq!(legacy.streaming(false), 0);
}
