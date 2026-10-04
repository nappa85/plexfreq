use plexfreq_core::model::Stream;
use serde_json::json;

#[test]
fn plex_analysis_gain_variants_keep_lyric_streams_readable() {
    for (value, expected) in [
        (json!(-6.5), Some(-6.5)),
        (json!("-6.5"), Some(-6.5)),
        (json!(null), None),
        (json!("NaN"), None),
        (json!("unknown"), None),
    ] {
        let stream: Stream = serde_json::from_value(
            json!({"key":"/lyrics/42","streamType":4,"gain":value,"albumGain":"-3.25"}),
        )
        .unwrap();
        assert_eq!(stream.gain, expected);
        assert_eq!(stream.album_gain, Some(-3.25));
        assert_eq!(stream.key, "/lyrics/42");
    }
}
