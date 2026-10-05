use plexfreq_core::{model::Item, queue::{Queue, Repeat}};
fn track(id: &str) -> Item { Item { rating_key: id.into(), kind: "track".into(), duration: 120000, ..Default::default() } }
fn main() {
    // numeric
    let long = "1".repeat(100);
    println!("numeric long ok? {:?}", plexfreq_core::numeric(&long).is_ok());
    // queue advance after end with repeat All
    let mut q = Queue::default();
    q.replace(vec![track("0"), track("1")], 0).unwrap();
    q.advance(false);
    let end = q.advance(false);
    println!("end {:?} track {:?}", end.map(|i| i.rating_key.clone()), q.track().map(|i| i.rating_key.clone()));
    q.repeat = Repeat::All;
    let after = q.advance(false);
    println!("after repeat All advance: {:?}", after.map(|i| i.rating_key.clone()));
    // lenient i32
    let payload = serde_json::json!({"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"1","key":"/a","title":"T","type":"track","year":99999999999u64}]}});
    let env: plexfreq_core::model::Envelope = serde_json::from_value(payload).unwrap();
    println!("year 99999999999 -> {}", env.container.items[0].year);
    let payload2 = serde_json::json!({"MediaContainer":{"size":1,"Metadata":[{"ratingKey":"1","key":"/a","title":"T","type":"track","year":18446744073709551615u64}]}});
    // serde_json can't represent u64::MAX as number? try via string?
    println!("payload2 test skipped");
    // presentation corrupt
    let mut q2 = Queue::default();
    q2.replace(vec![track("0"), track("1")], 0).unwrap();
    let mut v = serde_json::to_value(&q2).unwrap();
    v["order"] = serde_json::json!([1,0]);
    v["current"] = serde_json::json!(0);
    v["cursor"] = serde_json::json!(0);
    let corrupt: Queue = serde_json::from_value(v).unwrap();
    let p = corrupt.presentation();
    println!("corrupt presentation current {:?} items {:?}", p.current, p.items.iter().map(|i| i.rating_key.clone()).collect::<Vec<_>>());
    // offline_search empty
    // page_sized hasMore tested via integration, skip here
}
