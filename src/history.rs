use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    pub namespace: String,
    pub key: String,
    pub occurrence: String,
    pub played_at: u64,
    pub delivered: bool,
}
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn qualifies(listened: u64, duration: u64) -> bool {
    duration > 0 && listened >= duration.div_ceil(2).min(240000)
}

#[cfg(test)]
mod tests {
    #[test]
    fn listened_time_not_seek_position_qualifies_a_play() {
        assert!(!super::qualifies(1000, 180000));
        assert!(super::qualifies(90000, 180000));
        assert!(!super::qualifies(0, 0));
    }
}
