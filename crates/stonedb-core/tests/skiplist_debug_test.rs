//! Minimal SkipList debug test

use bytes::Bytes;
use stonedb_core::{BytewiseComparator, SkipList};

fn make_skiplist() -> SkipList<BytewiseComparator> {
    SkipList::with_capacity(BytewiseComparator::new(), 1024 * 1024, false)
}

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

#[test]
fn test_simple_insert_and_get() {
    let sl = make_skiplist();

    println!("Inserting key1...");
    sl.put(b("key1"), b("value1"));

    println!("Getting key1...");
    let result = sl.get(&b("key1"));
    println!("Result: {:?}", result);

    assert!(result.is_some(), "key1 should be found");
    assert_eq!(&*result.unwrap(), b"value1");
}

#[test]
fn test_two_inserts() {
    let sl = make_skiplist();

    println!("\n=== Insert 1: key_a ===");
    sl.put(b("key_a"), b("value_a"));

    println!("\n=== Insert 2: key_b ===");
    sl.put(b("key_b"), b("value_b"));

    println!("\n=== Get key_a ===");
    let r1 = sl.get(&b("key_a"));
    println!("key_a result: {:?}", r1);

    println!("\n=== Get key_b ===");
    let r2 = sl.get(&b("key_b"));
    println!("key_b result: {:?}", r2);

    assert!(r1.is_some(), "key_a should be found");
    assert!(r2.is_some(), "key_b should be found");
}

#[test]
fn test_sequential_keys() {
    let sl = make_skiplist();

    // Insert keys in order
    for i in 0..5 {
        let key = format!("k{:02}", i);
        let value = format!("v{:02}", i);
        println!("Insert {} -> {}", key, value);
        sl.put(b(&key), b(&value));
    }

    // Check all keys
    for i in 0..5 {
        let key = format!("k{:02}", i);
        let value = format!("v{:02}", i);
        println!("Get {}...", key);
        let result = sl.get(&b(&key));
        println!("  result: {:?}", result);
        assert!(result.is_some(), "Key {} should be found", key);
        assert_eq!(&*result.unwrap(), value.as_bytes());
    }
}

#[test]
fn test_update_same_key() {
    let sl = make_skiplist();

    println!("Insert key -> value1");
    let r1 = sl.put(b("key"), b("value1"));
    println!("  put returned: {:?}", r1);

    println!("Insert key -> value2");
    let r2 = sl.put(b("key"), b("value2"));
    println!("  put returned: {:?}", r2);

    println!("Get key...");
    let result = sl.get(&b("key"));
    println!("Result: {:?}", result);

    // Insert-only semantics: the first put succeeds (None), the second reports a
    // conflict (Some) and the stored value is left unchanged. Nodes are immutable.
    assert!(r1.is_none(), "First insert should succeed");
    assert!(
        r2.is_some(),
        "Second insert with different value should return conflict, not succeed silently"
    );
    assert_eq!(
        &*result.unwrap(),
        b"value1",
        "Value must not be overwritten in place"
    );
}
