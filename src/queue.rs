use crate::{model::Item, Error, Result};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Copy, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

#[derive(Default, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Queue {
    pub items: Vec<Item>,
    pub current: Option<usize>,
    pub repeat: Repeat,
    pub shuffled: bool,
    order: Vec<usize>,
    cursor: usize,
}

impl Queue {
    pub fn validate(&self) -> Result<()> {
        let mut order = self.order.clone();
        order.sort_unstable();
        if self.items.len() > 10000
            || self.items.iter().any(|i| i.kind != "track")
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
            || self.items.len() + items.len() > 10000
            || items.iter().any(|i| i.kind != "track")
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
            shuffled: self.shuffled,
            ..Self::default()
        };
        // Never panic on corrupt persisted order: keep only addressable items
        // and clamp the playback slot. Worker threads must survive bad state.
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
            q.cursor = self.cursor.min(q.items.len() - 1);
            q.current = self.current.map(|_| q.cursor);
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
        if start >= items.len() || items.len() > 10000 || items.iter().any(|i| i.kind != "track") {
            return Err(Error::Input(
                "Queue requires tracks and a valid start index",
            ));
        }
        self.items = items;
        self.current = Some(start);
        self.reorder();
        Ok(())
    }

    fn reorder(&mut self) {
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
                    let slot = self.cursor.min(self.order.len() - 1);
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
            self.cursor = self.cursor.min(self.order.len() - 1);
        }
    }

    pub fn shuffle(&mut self, enabled: bool) {
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
        Ok(())
    }

    pub fn advance(&mut self, automatic: bool) -> Option<&Item> {
        self.current?;
        if self.order.is_empty() {
            self.current = None;
            self.cursor = 0;
            return None;
        }
        if automatic && self.repeat == Repeat::One {
            return self.track();
        }
        if self.cursor + 1 < self.order.len() {
            self.cursor += 1;
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
        if self.order.is_empty() {
            return None;
        }
        if self.current.is_none() {
            // Natural end of queue: resume the last audible entry instead of
            // skipping it. `cursor` still points at the final playback slot.
            self.cursor = self.cursor.min(self.order.len() - 1);
            self.current = Some(self.order[self.cursor]);
            return self.track();
        }
        self.cursor = self.cursor.min(self.order.len() - 1).saturating_sub(1);
        self.current = Some(self.order[self.cursor]);
        self.track()
    }

    pub fn track(&self) -> Option<&Item> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn at_end(&self) -> bool {
        self.current.is_some() && self.cursor + 1 >= self.order.len()
    }
    pub fn upcoming(&self, count: usize) -> Vec<Item> {
        if self.current.is_none() {
            return Vec::new();
        }
        self.order
            .iter()
            .skip(self.cursor)
            .take(count)
            .filter_map(|i| self.items.get(*i).cloned())
            .collect()
    }

    pub fn append(&mut self, items: Vec<Item>) -> Result<()> {
        if items.is_empty() || items.iter().any(|i| i.kind != "track") {
            return Err(Error::Input("Radio returned no playable tracks"));
        }
        if self.items.len() + items.len() > 10000 {
            return Err(Error::Input("Choose tracks for the queue (maximum 10000)"));
        }
        self.order
            .extend(self.items.len()..self.items.len() + items.len());
        self.items.extend(items);
        Ok(())
    }

    pub fn trim_history(&mut self, keep: usize) {
        if self.order.is_empty() || self.current.is_none() {
            return;
        }
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
        let mut items = Vec::with_capacity(self.items.len() - dropped.len());
        for (old, item) in self.items.drain(..).enumerate() {
            if !dropped.contains(&old) {
                mapping[old] = items.len();
                items.push(item);
            }
        }
        self.items = items;
        let mut order = Vec::with_capacity(self.order.len() - drop);
        for old in self.order.iter().skip(drop) {
            let mapped = mapping.get(*old).copied().unwrap_or(usize::MAX);
            if mapped != usize::MAX {
                order.push(mapped);
            }
        }
        self.order = order;
        self.cursor -= drop;
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
            self.cursor = self.cursor.min(self.order.len() - 1);
        }
    }
}
