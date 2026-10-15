use crate::{model::Item, Error, Result};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(
    Default,
    Clone,
    Copy,
    Deserialize,
    Serialize,
    PartialEq,
    Debug,
    strum::Display,
    strum::EnumString,
    strum::IntoStaticStr,
    strum::EnumIter,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

pub const MAX_QUEUE: usize = 10_000;

#[derive(Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Queue {
    items: Vec<Item>,
    current: Option<usize>,
    repeat: Repeat,
    shuffled: bool,
    order: Vec<usize>,
    cursor: usize,
    /// Identity of this queue's content, replaced by every mutation. Random
    /// rather than counted so two independent queues (a cleared queue, a
    /// deserialized one) can never share a revision with different content.
    /// Persistence uses it to skip rewriting an unchanged queue.
    #[serde(skip, default = "fresh_revision")]
    revision: u64,
}

fn fresh_revision() -> u64 {
    rand::random()
}

impl Default for Queue {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            current: None,
            repeat: Repeat::default(),
            shuffled: false,
            order: Vec::new(),
            cursor: 0,
            revision: fresh_revision(),
        }
    }
}

impl Queue {
    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub fn current(&self) -> Option<usize> {
        self.current
    }
    pub fn repeat(&self) -> Repeat {
        self.repeat
    }
    pub fn shuffled(&self) -> bool {
        self.shuffled
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    /// Re-attach the revision a persisted queue was saved under.
    pub(crate) fn restore_revision(&mut self, revision: u64) {
        self.revision = revision;
    }
    fn touch(&mut self) {
        self.revision = fresh_revision();
    }
    pub fn set_repeat(&mut self, repeat: Repeat) {
        if self.repeat != repeat {
            self.repeat = repeat;
            self.touch();
        }
    }
    /// Replace the current track's metadata (e.g. with its cached or resolved
    /// form). Returns the replaced item, or None without a current track.
    pub fn replace_track(&mut self, item: Item) -> Option<Item> {
        let index = self.current?;
        let slot = self.items.get_mut(index)?;
        let old = std::mem::replace(slot, item);
        self.touch();
        Some(old)
    }
    /// Edit item metadata in place (e.g. ratings, stripped artwork).
    pub fn update_items(&mut self, mut edit: impl FnMut(&mut Item)) {
        self.items.iter_mut().for_each(&mut edit);
        self.touch();
    }
    pub fn validate(&self) -> Result<()> {
        let mut order = self.order.clone();
        order.sort_unstable();
        if self.items.len() > MAX_QUEUE
            || self
                .items
                .iter()
                .any(|i| i.kind != crate::model::ItemKind::Track)
            || order != (0..self.items.len()).collect::<Vec<_>>()
            || self
                .current
                .is_some_and(|i| self.order.get(self.cursor) != Some(&i))
        {
            return Err(Error::Input("Saved queue is invalid"));
        }
        // Cursor must always address a live playback slot, even after natural
        // end (current == None retains the last slot for previous()). Without
        // this a corrupt persisted cursor survives restarts.
        if self.order.is_empty() {
            if self.cursor != 0 {
                return Err(Error::Input("Saved queue is invalid"));
            }
        } else if self.cursor >= self.order.len() {
            return Err(Error::Input("Saved queue is invalid"));
        }
        Ok(())
    }
    pub fn insert(&mut self, items: Vec<Item>, next: bool) -> Result<()> {
        if items.is_empty()
            || self.items.len() + items.len() > MAX_QUEUE
            || items
                .iter()
                .any(|i| i.kind != crate::model::ItemKind::Track)
        {
            return Err(Error::Input("Choose tracks for the queue (maximum 10000)"));
        }
        let first = self.items.len();
        let count = items.len();
        let at = if next && self.current.is_some() {
            if self.cursor >= self.order.len() {
                return Err(Error::Input("Saved queue is invalid"));
            }
            self.cursor + 1
        } else {
            self.order.len()
        };
        self.items.extend(items);
        self.order.splice(at..at, first..first + count);
        self.touch();
        Ok(())
    }
    pub fn remove(&mut self, index: usize) -> Result<bool> {
        if index >= self.items.len() {
            return Err(Error::Input("Invalid queue index"));
        }
        let active = self.current == Some(index);
        let position = self
            .order
            .iter()
            .position(|i| *i == index)
            .ok_or(Error::Input("Invalid queue order"))?;
        self.touch();
        self.items.remove(index);
        self.order.remove(position);
        for i in &mut self.order {
            if *i > index {
                *i -= 1;
            }
        }
        if active {
            self.cursor = position.min(self.order.len().saturating_sub(1));
            self.current = self.order.get(position).copied();
        } else {
            if position < self.cursor {
                self.cursor = self.cursor.saturating_sub(1);
            }
            self.current = self.current.map(|i| if i > index { i - 1 } else { i });
        }
        self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
        Ok(active)
    }
    pub fn move_item(&mut self, from: usize, to: usize) -> Result<()> {
        // Manual reorder exits shuffle: the displayed (playback) order becomes
        // the new physical order. Covered by queue_edit regression.
        if from >= self.items.len() || to >= self.items.len() {
            return Err(Error::Input("Invalid queue index"));
        }
        if from == to {
            return Ok(());
        }
        self.touch();
        let remap = |i| {
            if i == from {
                to
            } else if from < to && i > from && i <= to {
                i - 1
            } else if to < from && i >= to && i < from {
                i + 1
            } else {
                i
            }
        };
        let current = self.current.map(remap);
        if self.current.is_none() {
            if let Some(last) = self.order.get_mut(self.cursor) {
                *last = remap(*last);
            }
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.current = current;
        self.shuffled = false;
        self.reorder();
        Ok(())
    }
    pub fn presentation(&self) -> Self {
        let mut q = Self {
            repeat: self.repeat,
            ..Self::default()
        };
        // This editable copy has materialized playback order, not shuffle policy.
        // UI snapshots must read the policy from the original queue.
        // Never panic on corrupt persisted order: keep only addressable items
        // and clamp the playback slot. Worker threads must survive bad state.
        // A dangling current (pointing outside items/order) must not be
        // invented as a valid selection pointing at the wrong track.
        q.items = self
            .order
            .iter()
            .filter_map(|i| self.items.get(*i).cloned())
            .collect();
        if q.items.is_empty() {
            q.current = None;
            q.cursor = 0;
            q.order = Vec::new();
        } else {
            // Map the physical current index through the (filtered) old order
            // so a corrupt cursor cannot invent a selection pointing at the
            // wrong track. q.items preserves old order; its index of the
            // current physical id is the correct new cursor.
            let filtered: Vec<usize> = self
                .order
                .iter()
                .filter(|i| self.items.get(**i).is_some())
                .copied()
                .collect();
            let pos = self
                .current
                .and_then(|current| filtered.iter().position(|i| *i == current));
            if let Some(pos) = pos {
                q.cursor = pos.min(q.items.len().saturating_sub(1));
                q.current = Some(q.cursor);
            } else {
                q.cursor = self.cursor.min(q.items.len().saturating_sub(1));
                q.current = None;
            }
            q.order = (0..q.items.len()).collect();
        }
        q
    }
    pub fn physical_index(&self, index: usize) -> Result<usize> {
        self.order
            .get(index)
            .copied()
            .ok_or(Error::Input("Invalid queue index"))
    }
    pub fn replace(&mut self, items: Vec<Item>, start: usize) -> Result<()> {
        if start >= items.len()
            || items.len() > MAX_QUEUE
            || items
                .iter()
                .any(|i| i.kind != crate::model::ItemKind::Track)
        {
            return Err(Error::Input(
                "Queue requires tracks and a valid start index",
            ));
        }
        self.items = items;
        self.current = Some(start);
        self.reorder();
        self.touch();
        Ok(())
    }

    fn reorder(&mut self) {
        if self.current.is_some_and(|index| index >= self.items.len()) {
            self.current = None;
        }
        let ended = if self.current.is_none() {
            self.order.get(self.cursor).copied()
        } else {
            None
        };
        self.order = (0..self.items.len()).collect();
        if self.shuffled {
            self.order.shuffle(&mut rand::thread_rng());
            if let Some(current) = self.current {
                if let Some(p) = self.order.iter().position(|i| *i == current) {
                    self.order.swap(0, p);
                } else {
                    // Corrupt persisted order: drop the dangling selection
                    // instead of panicking the worker thread.
                    self.current = None;
                }
            } else if let Some(last) = ended {
                if let Some(p) = self.order.iter().position(|i| *i == last) {
                    let slot = self.cursor.min(self.order.len().saturating_sub(1));
                    self.order.swap(slot, p);
                }
            }
        }
        // Preserve the natural-end slot (current == None retains the last
        // audible position for previous()). Resetting to 0 here would make a
        // shuffle/move at queue end resume the first row instead of the last.
        let preserved = ended
            .and_then(|last| self.order.iter().position(|i| *i == last))
            .unwrap_or(self.cursor);
        self.cursor = match self.current {
            Some(c) => self.order.iter().position(|i| *i == c).unwrap_or(preserved),
            None => preserved,
        };
        if self.order.is_empty() {
            self.cursor = 0;
        } else {
            self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
        }
    }

    pub fn shuffle(&mut self, enabled: bool) {
        self.touch();
        self.shuffled = enabled;
        self.reorder();
    }

    pub fn select(&mut self, index: usize) -> Result<()> {
        if index >= self.items.len() {
            return Err(Error::Input("Invalid queue index"));
        }
        let position = self
            .order
            .iter()
            .position(|i| *i == index)
            .ok_or(Error::Input("Invalid queue order"))?;
        self.current = Some(index);
        self.cursor = position;
        self.touch();
        Ok(())
    }

    pub fn advance(&mut self, automatic: bool) -> Option<&Item> {
        self.touch();
        if self.current.is_none() {
            // Natural end retains the last slot for previous(). Tracks
            // enqueued afterwards sit past that slot (upcoming() lists them),
            // so advance into them first. Otherwise enabling repeat-all
            // afterwards must allow a restart instead of staying stuck; both
            // manual and automatic advancement wrap.
            if self.cursor.saturating_add(1) < self.order.len() {
                self.cursor += 1;
                self.current = self.order.get(self.cursor).copied();
                return self.track();
            }
            if self.repeat == Repeat::All && !self.order.is_empty() {
                self.cursor = 0;
                self.current = self.order.first().copied();
                return self.track();
            }
            return None;
        }
        if self.order.is_empty() {
            self.current = None;
            self.cursor = 0;
            return None;
        }
        if automatic && self.repeat == Repeat::One {
            return self.track();
        }
        if self.cursor.saturating_add(1) < self.order.len() {
            self.cursor = self.cursor.saturating_add(1);
        } else if self.repeat == Repeat::All {
            self.cursor = 0;
        } else {
            self.current = None;
            return None;
        }
        self.current = self.order.get(self.cursor).copied();
        self.track()
    }

    pub fn previous(&mut self) -> Option<&Item> {
        self.touch();
        if self.order.is_empty() {
            return None;
        }
        if self.current.is_none() {
            // Natural end of queue: resume the last audible entry instead of
            // skipping it. `cursor` still points at the final playback slot.
            self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
            self.current = Some(self.order[self.cursor]);
            return self.track();
        }
        self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
        self.cursor = if self.cursor == 0 && self.repeat == Repeat::All {
            self.order.len() - 1
        } else {
            self.cursor.saturating_sub(1)
        };
        self.current = Some(self.order[self.cursor]);
        self.track()
    }

    pub fn track(&self) -> Option<&Item> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn at_end(&self) -> bool {
        self.current.is_some() && self.cursor.saturating_add(1) >= self.order.len()
    }
    pub fn upcoming(&self, count: usize) -> Vec<Item> {
        // After natural end (current == None) newly enqueued tracks sit past
        // the last audible slot. Returning [] here leaves them unreachable for
        // prefetch and UI; list the suffix past the retained cursor instead.
        // With no new tracks the suffix is empty, preserving old behavior.
        let skip = if self.current.is_none() {
            self.cursor.saturating_add(1)
        } else {
            self.cursor
        };
        self.order
            .iter()
            .skip(skip)
            .take(count)
            .filter_map(|i| self.items.get(*i).cloned())
            .collect()
    }

    pub fn append(&mut self, items: Vec<Item>) -> Result<()> {
        if items.is_empty()
            || items
                .iter()
                .any(|i| i.kind != crate::model::ItemKind::Track)
        {
            return Err(Error::Input("Radio returned no playable tracks"));
        }
        if self.items.len() + items.len() > MAX_QUEUE {
            return Err(Error::Input("Choose tracks for the queue (maximum 10000)"));
        }
        self.order
            .extend(self.items.len()..self.items.len() + items.len());
        self.items.extend(items);
        self.touch();
        Ok(())
    }

    pub fn trim_history(&mut self, keep: usize) {
        if self.order.is_empty() {
            return;
        }
        // Any change below (even clamping a corrupt cursor) is a new revision.
        self.touch();
        self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
        let drop = self.cursor.saturating_sub(keep);
        if drop == 0 {
            return;
        }
        // Drop the oldest playback-order prefix. Works for both linear and
        // shuffled orders; the old linear-only path leaked memory for radio.
        let dropped: std::collections::BTreeSet<usize> =
            self.order.iter().take(drop).copied().collect();
        let mut mapping = vec![usize::MAX; self.items.len()];
        let mut items = Vec::with_capacity(self.items.len().saturating_sub(dropped.len()));
        for (old, item) in self.items.drain(..).enumerate() {
            if !dropped.contains(&old) {
                mapping[old] = items.len();
                items.push(item);
            }
        }
        self.items = items;
        let mut order = Vec::with_capacity(self.order.len().saturating_sub(drop));
        for old in self.order.iter().skip(drop) {
            let mapped = mapping.get(*old).copied().unwrap_or(usize::MAX);
            if mapped != usize::MAX {
                order.push(mapped);
            }
        }
        self.order = order;
        self.cursor = self.cursor.saturating_sub(drop);
        if let Some(current) = self.current {
            self.current = match mapping.get(current).copied() {
                Some(mapped) if mapped != usize::MAX => Some(mapped),
                // Current is never in the dropped prefix, but never panic on
                // corrupt state: stop instead of pointing out of bounds.
                _ => None,
            };
        }
        if self.order.is_empty() {
            self.cursor = 0;
            self.current = None;
        } else {
            self.cursor = self.cursor.min(self.order.len().saturating_sub(1));
        }
    }
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    fn track(id: &str) -> Item {
        Item {
            rating_key: id.into(),
            kind: crate::model::ItemKind::Track,
            ..Default::default()
        }
    }

    fn queue() -> Queue {
        let mut q = Queue::default();
        q.replace(vec![track("0"), track("1"), track("2")], 0)
            .unwrap();
        q
    }

    #[test]
    fn every_mutation_replaces_the_revision_and_reads_keep_it() {
        type Edit = fn(&mut Queue);
        let edits: [(&str, Edit); 11] = [
            ("insert", |q| q.insert(vec![track("3")], true).unwrap()),
            ("append", |q| q.append(vec![track("3")]).unwrap()),
            ("remove", |q| {
                q.remove(2).unwrap();
            }),
            ("move", |q| q.move_item(0, 2).unwrap()),
            ("shuffle", |q| q.shuffle(true)),
            ("select", |q| q.select(2).unwrap()),
            ("advance", |q| {
                q.advance(false);
            }),
            ("previous", |q| {
                q.previous();
            }),
            ("repeat", |q| q.set_repeat(Repeat::All)),
            ("replace_track", |q| {
                q.replace_track(track("9"));
            }),
            ("update_items", |q| q.update_items(|i| i.user_rating = 10.)),
        ];
        for (name, edit) in edits {
            let mut q = queue();
            let before = q.revision();
            edit(&mut q);
            assert_ne!(before, q.revision(), "{name} kept the revision");
        }
        let q = queue();
        let before = q.revision();
        let _ = (
            q.items(),
            q.current(),
            q.track(),
            q.upcoming(5),
            q.presentation(),
        );
        assert_eq!(before, q.revision());
        assert_eq!(before, q.clone().revision(), "clones share content");
        let mut same = q.clone();
        same.set_repeat(Repeat::Off);
        same.move_item(1, 1).unwrap();
        assert_eq!(before, same.revision(), "no-op edits keep the revision");
    }

    #[test]
    fn independent_queues_never_share_a_revision() {
        let restored: Queue =
            serde_json::from_value(serde_json::to_value(queue()).unwrap()).unwrap();
        assert_ne!(Queue::default().revision(), Queue::default().revision());
        assert_ne!(queue().revision(), restored.revision());
    }
}
