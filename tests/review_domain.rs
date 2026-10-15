//! Domain review regressions (queue, lyrics, model). Each test fails on the
//! current implementation and documents the defect it pins down.
use plexfreq_core::{lyrics, model::Item, queue::Queue};

fn track(id: &str) -> Item {
    Item {
        rating_key: id.into(),
        kind: plexfreq_core::model::ItemKind::Track,
        duration: 120000,
        ..Default::default()
    }
}

// Queue::advance returns None whenever current == None unless repeat-all is
// set. After a natural end, upcoming() advertises newly enqueued tracks (see
// production_audit), but pressing Next never reaches them: advance() ignores
// the retained cursor and the slots after it.
#[test]
fn queue_next_after_natural_end_plays_newly_enqueued_track() {
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    assert!(q.advance(true).is_some());
    assert!(q.advance(true).is_none(), "natural end");
    q.insert(vec![track("2")], false).unwrap();
    assert_eq!(q.upcoming(5).len(), 1, "UI lists the new track as upcoming");
    let next = q.advance(false).map(|i| i.rating_key.clone());
    assert_eq!(
        next.as_deref(),
        Some("2"),
        "Next after enqueue at natural end must play the enqueued track"
    );
}

// LRC [offset:] semantics: a positive value makes lyrics appear sooner
// (shift timestamps earlier), negative later. parse() adds the offset, so
// every lyric file carrying an offset is shifted twice as far in the wrong
// direction (a +500 file ends up 1s late relative to the intended timing).
#[test]
fn lyrics_positive_lrc_offset_shows_lines_sooner() {
    let lines = lyrics::parse("[offset:+500]\n[00:01.00]Line");
    assert_eq!(lines[0].time, Some(500));
    let lines = lyrics::parse("[offset:-500]\n[00:01.00]Line");
    assert_eq!(lines[0].time, Some(1500));
}

// is_metadata_tag() treats any bracketed text containing ':' as an LRC ID
// tag. Common plain-lyric section annotations such as "[Verse 1: Artist]" or
// "[Chorus: Featured Singer]" are therefore silently dropped. LRC ID tags are
// a short alphanumeric identifier followed by ':' (ar, ti, al, by, length...).
#[test]
fn lyrics_section_annotation_with_colon_is_content() {
    let lines = lyrics::parse("[Verse 1: Fixture Artist]\nHello\n[ar:Fixture]");
    let texts: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec!["[Verse 1: Fixture Artist]", "Hello"]);
}

// parse() sorts by time.unwrap_or(0), so an untimed line inside or after a
// timed LRC body (a "[Chorus]" marker, a trailing credit line) is moved to
// the very top of the lyrics instead of staying where the author put it.
#[test]
fn lyrics_untimed_lines_keep_their_position_among_timed_lines() {
    let lines = lyrics::parse("[00:05.00]First\n[Chorus]\n[00:10.00]Second\nCredits");
    let texts: Vec<_> = lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, vec!["First", "[Chorus]", "Second", "Credits"]);
}

// f64_to_u64 accepts v == u64::MAX as f64 (== 2^64, out of range) and `as`
// saturates it to u64::MAX, contradicting the stated "out-of-range numbers
// must default instead of clamping" rule (lyrics::timestamp uses `<` for the
// same reason). A corrupt duration then becomes u64::MAX and overflows sums.
#[test]
fn model_out_of_range_float_duration_defaults_instead_of_saturating() {
    for duration in [
        serde_json::json!(18446744073709551616.0),
        serde_json::json!("18446744073709551616"),
    ] {
        let item: Item =
            serde_json::from_value(serde_json::json!({"type":"track","duration":duration}))
                .unwrap();
        assert_eq!(item.duration, 0, "2^64 must not saturate to u64::MAX");
    }
}
