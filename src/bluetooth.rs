//! Rootless audio-accessory disconnect policy, independent of the metadata actor.
use std::collections::BTreeSet;

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    pub connected: BTreeSet<String>,
    pub active: BTreeSet<String>,
    pub transports: BTreeSet<String>,
    pub idle_transports: usize,
    pub pending_transports: usize,
    pub active_transports: usize,
}
#[derive(Default)]
pub(crate) struct PausePolicy {
    armed: BTreeSet<String>,
    armed_transports: BTreeSet<String>,
}
impl PausePolicy {
    pub fn observe(&mut self, facts: Option<Facts>, playing: bool) -> bool {
        let Some(facts) = facts else { return false };
        let route_lost: BTreeSet<_> = self
            .armed_transports
            .difference(&facts.transports)
            .cloned()
            .collect();
        let lost = !route_lost.is_empty()
            || self
                .armed
                .iter()
                .any(|device| !facts.connected.contains(device));
        self.armed.retain(|device| facts.connected.contains(device));
        self.armed_transports
            .retain(|device| facts.transports.contains(device));
        if !facts.active.is_empty() {
            self.armed = facts
                .active
                .intersection(&facts.connected)
                .cloned()
                .collect();
        } else if self.armed.is_empty() && facts.connected.len() == 1 {
            self.armed = facts.connected.clone();
        }
        self.armed_transports
            .retain(|device| self.armed.contains(device));
        self.armed_transports
            .extend(self.armed.intersection(&facts.transports).cloned());
        // The audio profile can disappear while BLE/HFP keeps Device1 connected.
        // An existing idle transport is deliberately retained, not treated as loss.
        for device in route_lost {
            self.armed.remove(&device);
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
    #[test]
    fn audio_profile_removal_pauses_even_while_device_stays_connected() {
        let mut policy = PausePolicy::default();
        let mut car = facts(&["car", "headset"], &["car"]).unwrap();
        car.transports.insert("car".into());
        assert!(!policy.observe(Some(car.clone()), true));
        car.active.clear(); // Idle is not disconnect.
        assert!(!policy.observe(Some(car.clone()), true));
        assert!(!policy.observe(None, true));
        car.transports.clear();
        assert!(policy.observe(Some(car.clone()), true));
        assert!(!policy.observe(Some(car), true));
        assert!(!policy.observe(facts(&["headset"], &[]), false));
    }
    #[test]
    fn switching_accessory_forgets_the_previous_audio_route() {
        let mut policy = PausePolicy::default();
        let mut both = facts(&["car", "headset"], &["car"]).unwrap();
        both.transports = ["car".into(), "headset".into()].into_iter().collect();
        assert!(!policy.observe(Some(both.clone()), true));
        both.active = ["headset".into()].into_iter().collect();
        assert!(!policy.observe(Some(both.clone()), true));
        both.transports.remove("car");
        assert!(!policy.observe(Some(both), true));
    }
}
