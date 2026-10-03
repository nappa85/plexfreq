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
        self.items.extend(items);
        let at = if next && self.current.is_some() {
            self.cursor + 1
        } else {
            self.order.len()
        };
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
        Ok(active)
    }
    pub fn move_item(&mut self, from: usize, to: usize) -> Result<()> {
        if from >= self.items.len() || to >= self.items.len() {
            return Err(Error::Input("Invalid queue index"));
        }
        let current = self.current.map(|i| {
            if i == from {
                to
            } else if from < to && i > from && i <= to {
                i - 1
            } else if to < from && i >= to && i < from {
                i + 1
            } else {
                i
            }
        });
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.current = current;
        self.shuffled = false;
        self.reorder();
        Ok(())
    }
    pub fn presentation(&self) -> Self {
        let mut q = self.clone();
        q.items = self.order.iter().map(|i| self.items[*i].clone()).collect();
        q.current = self.current.map(|_| self.cursor);
        q.order = (0..q.items.len()).collect();
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
        self.order = (0..self.items.len()).collect();
        if self.shuffled {
            self.order.shuffle(&mut rand::thread_rng());
            if let Some(current) = self.current {
                let p = self.order.iter().position(|i| *i == current).unwrap();
                self.order.swap(0, p);
            }
        }
        self.cursor = self
            .current
            .and_then(|c| self.order.iter().position(|i| *i == c))
            .unwrap_or(0);
    }

    pub fn shuffle(&mut self, enabled: bool) {
        self.shuffled = enabled;
        self.reorder();
    }

    pub fn select(&mut self, index: usize) -> Result<()> {
        if index >= self.items.len() {
            return Err(Error::Input("Invalid queue index"));
        }
        self.current = Some(index);
        self.cursor = self.order.iter().position(|i| *i == index).unwrap();
        Ok(())
    }

    pub fn advance(&mut self, automatic: bool) -> Option<&Item> {
        self.current?;
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
        self.current = Some(self.order[self.cursor]);
        self.track()
    }

    pub fn previous(&mut self) -> Option<&Item> {
        if self.order.is_empty() {
            return None;
        }
        self.cursor = self.cursor.saturating_sub(1);
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
        self.order
            .extend(self.items.len()..self.items.len() + items.len());
        self.items.extend(items);
        Ok(())
    }

    pub fn trim_history(&mut self, keep: usize) {
        if !self.shuffled {
            if let Some(current) = self.current {
                let remove = current.saturating_sub(keep);
                self.items.drain(..remove);
                self.current = Some(current - remove);
                self.reorder();
            }
        }
    }
}
