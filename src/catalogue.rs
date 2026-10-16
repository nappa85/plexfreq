//! Reusable normalized offline catalogue. Rebuilt on snapshot/cache/plan changes.
//!
//! Search is indexed and bounded. A query term has no whitespace, so it is a
//! substring of an item's text exactly when it is a substring of one of the
//! text's whitespace-separated words: matching walks the distinct-word
//! vocabulary, not the items. The most selective term's posting lists are
//! merged in catalogue order and the other terms are checked on those rows
//! only. A page stops as soon as it is full (plus one row to know whether more
//! follow), never counts past [`MAX_RESULTS`] and examines at most
//! [`MAX_EXAMINED`] candidate rows, so deep or broad queries cannot turn one
//! request into a whole-catalogue scan. Pages are deterministic: every page of
//! a query is cut from the same capped result sequence.
use crate::model::{Item, ItemKind};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap},
    time::SystemTime,
};

/// Most matches a query offers across all its pages.
pub(crate) const MAX_RESULTS: usize = 1000;
/// Most candidate rows one page request examines.
pub(crate) const MAX_EXAMINED: usize = 20_000;
/// Query terms whose vocabulary matches are computed (the longest, usually
/// the most selective); all terms are still checked on candidate rows.
const MAX_INDEXED_TERMS: usize = 8;

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
    /// Distinct lowercase words -> ascending item rows containing them.
    words: BTreeMap<String, Vec<u32>>,
}
/// One page of a bounded query.
pub(crate) struct Page<'a> {
    pub items: Vec<&'a Item>,
    pub has_more: bool,
    /// The result sequence was cut by [`MAX_RESULTS`] or [`MAX_EXAMINED`]:
    /// more matches may exist than are offered; refine the query.
    pub truncated: bool,
    /// Candidate rows examined (work done) for this page.
    pub examined: usize,
}
impl Index {
    pub fn new(stamp: Stamp, mut items: Vec<Item>) -> Self {
        items
            .sort_by_cached_key(|i| (i.title.to_lowercase(), i.kind.clone(), i.rating_key.clone()));
        let text: Vec<String> = items
            .iter()
            .map(|i| {
                format!("{} {} {}", i.title, i.parent_title, i.grandparent_title).to_lowercase()
            })
            .collect();
        let mut words: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (row, text) in text.iter().enumerate() {
            let row = u32::try_from(row).unwrap_or(u32::MAX);
            for word in text.split_whitespace() {
                let rows = words.entry(word.to_owned()).or_default();
                // Rows are visited in ascending order: dedup is a tail check.
                if rows.last() != Some(&row) {
                    rows.push(row);
                }
            }
        }
        Self {
            stamp,
            items,
            text,
            words,
        }
    }
    /// The page of `query` matches (in catalogue order) starting at match
    /// `start`, at most `size` items.
    pub fn search(&self, query: &str, kind: &ItemKind, start: usize, size: usize) -> Page<'_> {
        let mut page = Page {
            items: Vec::new(),
            has_more: false,
            truncated: false,
            examined: 0,
        };
        let mut terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        terms.sort();
        terms.dedup();
        if terms.is_empty() || size == 0 {
            return page;
        }
        // Posting lists per indexed term; the driver is the term with the
        // fewest postings. A term matching no word means no results.
        let mut indexed: Vec<&String> = terms.iter().collect();
        indexed.sort_by_key(|term| Reverse(term.len()));
        indexed.truncate(MAX_INDEXED_TERMS);
        let mut driver: Option<Vec<&[u32]>> = None;
        let mut driver_size = usize::MAX;
        for term in indexed {
            let lists: Vec<&[u32]> = self
                .words
                .iter()
                .filter(|(word, _)| word.contains(term.as_str()))
                .map(|(_, rows)| rows.as_slice())
                .collect();
            let postings: usize = lists.iter().map(|rows| rows.len()).sum();
            if postings == 0 {
                return page;
            }
            if postings < driver_size {
                driver_size = postings;
                driver = Some(lists);
            }
        }
        let Some(lists) = driver else {
            return page;
        };
        // K-way merge of the driver's posting lists in ascending row order.
        let mut heap: BinaryHeap<Reverse<(u32, usize, usize)>> = lists
            .iter()
            .enumerate()
            .filter_map(|(list, rows)| rows.first().map(|&row| Reverse((row, list, 0))))
            .collect();
        let mut previous = None;
        let mut matched = 0usize;
        while let Some(Reverse((row, list, position))) = heap.pop() {
            if let Some(&next) = lists[list].get(position + 1) {
                heap.push(Reverse((next, list, position + 1)));
            }
            if previous == Some(row) {
                continue;
            }
            previous = Some(row);
            if page.examined == MAX_EXAMINED {
                page.truncated = true;
                break;
            }
            page.examined += 1;
            let (item, text) = (&self.items[row as usize], &self.text[row as usize]);
            if !(kind.is_empty() || item.kind == *kind)
                || !terms.iter().all(|term| text.contains(term.as_str()))
            {
                continue;
            }
            if matched == MAX_RESULTS {
                page.truncated = true;
                break;
            }
            if matched >= start.saturating_add(size) {
                page.has_more = true;
                break;
            }
            if matched >= start {
                page.items.push(item);
            }
            matched += 1;
        }
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp() -> Stamp {
        Stamp {
            namespace: String::new(),
            library: None,
            audio: 0,
            plans: 0,
        }
    }
    /// Deterministic synthetic catalogue: artists, albums and tracks with
    /// shared and unique words.
    fn catalogue(tracks: usize) -> Vec<Item> {
        let mut items = Vec::new();
        for n in 0..tracks {
            let (artist, album) = (n % 97, n % 1009);
            items.push(Item {
                kind: ItemKind::Track,
                rating_key: n.to_string(),
                title: format!("Song{n} {}", ["Blue", "Red", "Green"][n % 3]),
                parent_title: format!("Album{album}"),
                grandparent_title: format!("Artist{artist} The Band"),
                ..Default::default()
            });
        }
        for album in 0..1009 {
            items.push(Item {
                kind: ItemKind::Album,
                rating_key: format!("a{album}"),
                title: format!("Album{album}"),
                parent_title: format!("Artist{} The Band", album % 97),
                ..Default::default()
            });
        }
        items
    }
    fn brute<'a>(index: &'a Index, query: &str, kind: &ItemKind) -> Vec<&'a Item> {
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        index
            .items
            .iter()
            .zip(&index.text)
            .filter(|(item, text)| {
                !terms.is_empty()
                    && (kind.is_empty() || item.kind == *kind)
                    && terms.iter().all(|term| text.contains(term))
            })
            .map(|(item, _)| item)
            .collect()
    }
    fn keys(items: &[&Item]) -> Vec<String> {
        items.iter().map(|i| i.rating_key.clone()).collect()
    }

    #[test]
    fn indexed_search_matches_substring_semantics_and_pages_correctly() {
        let index = Index::new(stamp(), catalogue(5000));
        let all = ItemKind::default();
        for (query, kind) in [
            ("song12 blue", &all),
            ("ALBUM10", &all),
            ("um100", &ItemKind::Album),
            ("artist5 green", &ItemKind::Track),
            ("the band ong4", &all),
            ("nothing-like-this", &all),
        ] {
            let expected = brute(&index, query, kind);
            let mut paged = Vec::new();
            let mut start = 0;
            loop {
                let page = index.search(query, kind, start, 100);
                assert!(page.items.len() <= 100);
                start += page.items.len();
                paged.extend(page.items);
                if !page.has_more {
                    assert!(page.truncated || paged.len() == expected.len(), "{query}");
                    break;
                }
            }
            let offered = expected.len().min(MAX_RESULTS);
            assert_eq!(keys(&paged), keys(&expected[..offered]), "{query}");
        }
    }

    // A large synthetic catalogue: a selective query examines only its
    // candidate rows, a broad one is capped per page and per query, and
    // paging stays consistent (no duplicates or gaps) up to the cap.
    #[test]
    fn large_catalogue_queries_do_bounded_work() {
        let index = Index::new(stamp(), catalogue(100_000));
        let all = ItemKind::default();
        let selective = index.search("song4242", &all, 0, 100);
        assert_eq!(
            keys(&selective.items),
            [
                "4242", "42420", "42421", "42422", "42423", "42424", "42425", "42426", "42427",
                "42428", "42429"
            ]
        );
        assert!(selective.examined <= 11, "examined {}", selective.examined);
        assert!(!selective.has_more);

        // Every track matches "the band": the page stops when full.
        let first = index.search("the band", &all, 0, 100);
        assert_eq!(first.items.len(), 100);
        assert!(first.has_more);
        assert!(first.examined <= 101, "examined {}", first.examined);

        // Pages through the broad query until the result cap.
        let expected = brute(&index, "the band", &all);
        let mut seen = Vec::new();
        let mut start = 0;
        loop {
            let page = index.search("the band", &all, start, 100);
            assert!(page.examined <= MAX_RESULTS + 1);
            start += page.items.len();
            seen.extend(page.items);
            if !page.has_more {
                assert!(page.truncated);
                break;
            }
        }
        assert_eq!(keys(&seen), keys(&expected[..MAX_RESULTS]));
        let beyond = index.search("the band", &all, MAX_RESULTS, 100);
        assert!(beyond.items.is_empty() && !beyond.has_more && beyond.truncated);

        // A kind filter that rejects nearly every candidate is capped too.
        let albums = index.search("blue", &ItemKind::Album, 0, 100);
        assert!(albums.items.is_empty());
        assert!(albums.truncated);
        assert_eq!(albums.examined, MAX_EXAMINED);
    }
}
