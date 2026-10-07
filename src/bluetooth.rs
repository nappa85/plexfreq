//! Rootless audio-accessory disconnect policy, independent of the metadata actor.
use std::collections::BTreeSet;

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    pub connected: BTreeSet<String>,
    pub active: BTreeSet<String>,
    pub idle_transports: usize,
    pub pending_transports: usize,
    pub active_transports: usize,
}
#[derive(Default)]
pub(crate) struct PausePolicy {
    armed: BTreeSet<String>,
}
impl PausePolicy {
    pub fn observe(&mut self, facts: Option<Facts>, playing: bool) -> bool {
        let Some(facts) = facts else { return false };
        let lost = self
            .armed
            .iter()
            .any(|device| !facts.connected.contains(device));
        self.armed.retain(|device| facts.connected.contains(device));
        if !facts.active.is_empty() {
            self.armed = facts
                .active
                .intersection(&facts.connected)
                .cloned()
                .collect();
        } else if self.armed.is_empty() && facts.connected.len() == 1 {
            self.armed = facts.connected;
        }
        lost && playing
    }
}
pub(crate) fn audio_uuid(uuid: &str) -> bool {
    [
        "0000110b-0000-1000-8000-00805f9b34fb",
        "00001108-0000-1000-8000-00805f9b34fb",
        "0000111e-0000-1000-8000-00805f9b34fb",
    ]
    .iter()
    .any(|audio| uuid.eq_ignore_ascii_case(audio))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn facts(connected: &[&str], active: &[&str]) -> Option<Facts> {
        Some(Facts {
            connected: connected.iter().map(|s| s.to_string()).collect(),
            active: active.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        })
    }
    #[test]
    fn disconnect_pauses_once_without_startup_or_poll_error_false_positives() {
        let mut policy = PausePolicy::default();
        assert!(!policy.observe(facts(&[], &[]), true));
        assert!(!policy.observe(facts(&["car"], &["car"]), true));
        assert!(!policy.observe(None, true));
        assert!(policy.observe(facts(&[], &[]), true));
        assert!(!policy.observe(facts(&[], &[]), true));
        assert!(!policy.observe(facts(&["car"], &["car"]), false));
        assert!(!policy.observe(facts(&[], &[]), false));
    }
    #[test]
    fn idle_transport_and_unrelated_device_loss_do_not_pause_music() {
        let mut policy = PausePolicy::default();
        assert!(!policy.observe(facts(&["car", "headset"], &["car"]), true));
        assert!(!policy.observe(facts(&["car"], &[]), true)); // not streaming is not a disconnect
        assert!(policy.observe(facts(&[], &[]), true));
        assert!(audio_uuid("0000110b-0000-1000-8000-00805f9b34fb"));
        assert!(!audio_uuid("00001812-0000-1000-8000-00805f9b34fb"));
    }
    #[test]
    fn device_is_tracked_before_play_starts_but_reconnect_never_resumes() {
        let mut policy = PausePolicy::default();
        assert!(!policy.observe(facts(&["car"], &[]), false));
        assert!(policy.observe(facts(&[], &[]), true));
        assert!(!policy.observe(facts(&["car"], &[]), false));
    }
}
