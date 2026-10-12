//! Shared page envelope; advance by server rows, not filtered display rows.
use serde_json::{json, Value};

pub(crate) fn page(
    items: Vec<Value>,
    start: usize,
    received: usize,
    size: usize,
    total: Option<usize>,
    offline: bool,
) -> Value {
    let next = start.saturating_add(received);
    let has_more = next > start && total.map_or(size > 0 && received >= size, |total| next < total);
    json!({"items":items,"start":start,"next":next,"hasMore":has_more,"offline":offline})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paging_advances_past_filtered_rows_but_never_loops_on_an_empty_page() {
        assert_eq!(page(vec![], 100, 100, 100, Some(300), false)["next"], 200);
        assert_eq!(page(vec![], 100, 100, 100, None, false)["hasMore"], true);
        assert_eq!(
            page(vec![], 100, 0, 100, Some(300), false)["hasMore"],
            false
        );
        assert_eq!(
            page(vec![], usize::MAX, 100, 100, None, true)["hasMore"],
            false
        );
    }
}
