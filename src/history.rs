use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    pub namespace: String,
    pub key: String,
    pub occurrence: String,
    pub played_at: u64,
    pub delivered: bool,
    /// The server already counted this play when playback started (Jellyfin
    /// counts at `Sessions/Playing`), so the replay must not count it again.
    /// Persisted so restoring a queue does not report the start twice. Absent
    /// in sessions saved before the field existed, and omitted when false so
    /// Plex history lines keep their bytes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub start_reported: bool,
}
pub fn qualifies(listened: u64, duration: u64) -> bool {
    duration > 0 && listened >= duration.div_ceil(2).min(240000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_play_saved_before_start_reporting_loads_as_not_reported() {
        let old = r#"{"namespace":"n","key":"1","occurrence":"o","playedAt":5,"delivered":false}"#;
        let play: super::Play = serde_json::from_str(old).unwrap();
        assert!(!play.start_reported);
        // An unreported play serialises to exactly the old bytes.
        assert_eq!(serde_json::to_string(&play).unwrap(), old);
        let reported = super::Play {
            start_reported: true,
            ..play
        };
        let line = serde_json::to_string(&reported).unwrap();
        assert!(line.ends_with(r#","startReported":true}"#), "{line}");
        let back: super::Play = serde_json::from_str(&line).unwrap();
        assert!(back.start_reported);
    }

    #[test]
    fn listened_time_not_seek_position_qualifies_a_play() {
        assert!(!super::qualifies(1000, 180000));
        assert!(super::qualifies(90000, 180000));
        assert!(!super::qualifies(0, 0));
    }
}
