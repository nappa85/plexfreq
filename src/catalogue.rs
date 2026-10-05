//! Reusable normalized offline catalogue. Rebuilt on snapshot/cache/plan changes.
use crate::model::Item;
use std::time::SystemTime;

#[derive(PartialEq)]
pub(crate) struct Stamp {
    pub namespace: String,
    pub library: Option<SystemTime>,
    pub audio: u64,
    pub plans: u64,
}
pub(crate) struct Index {
    pub stamp: Stamp,
    pub items: Vec<Item>,
    text: Vec<String>,
}
impl Index {
    pub fn new(stamp: Stamp, mut items: Vec<Item>) -> Self {
        items
            .sort_by_cached_key(|i| (i.title.to_lowercase(), i.kind.clone(), i.rating_key.clone()));
        let text = items
            .iter()
            .map(|i| {
                format!("{} {} {}", i.title, i.parent_title, i.grandparent_title).to_lowercase()
            })
            .collect();
        Self { stamp, items, text }
    }
    pub fn search<'a>(&'a self, query: &str, kind: &'a str) -> impl Iterator<Item = &'a Item> {
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        self.items
            .iter()
            .zip(&self.text)
            .filter(move |(item, text)| {
                (kind.is_empty() || item.kind == kind)
                    && terms.iter().all(|term| text.contains(term))
            })
            .map(|(item, _)| item)
    }
}
