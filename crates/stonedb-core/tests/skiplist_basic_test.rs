//! Basic SkipList Tests
//!
//! These tests verify basic SkipList functionality without concurrent access.

use bytes::Bytes;
use stonedb_core::{BytewiseComparator, SkipList};

fn make_skiplist() -> SkipList<BytewiseComparator> {
    SkipList::with_capacity(BytewiseComparator::new(), 1024 * 1024, false)
}

fn b(s: &str) -> Bytes {
    Bytes::copy_from_slice(s.as_bytes())
}

#[test]
fn test_single_insert() {
    let sl = make_skiplist();
    sl.put(b("key1"), b("value1"));
    let result = sl.get(&b("key1"));
    assert!(result.is_some());
    assert_eq!(&*result.unwrap(), b"value1");
}

#[test]
fn test_multiple_inserts() {
    let sl = make_skiplist();

    for i in 0..100 {
        let key = format!("key_{}", i);
        let value = format!("value_{}", i);
        sl.put(b(&key), b(&value));
    }

    // Verify all present
    for i in 0..100 {
        let key = format!("key_{}", i);
        let value = format!("value_{}", i);
        let result = sl.get(&b(&key));
        assert!(result.is_some(), "Key {} not found", i);
        assert_eq!(&*result.unwrap(), value.as_bytes());
    }
}

#[test]
fn test_put_existing_key_does_not_overwrite() {
    // Arrange: a key is already present.
    let sl = make_skiplist();
    sl.put(b("key"), b("value1"));

    // Act: put the same key with a different value.
    let conflict = sl.put(b("key"), b("value2"));

    // Assert: insert-only semantics (AgateDB) -- the conflict is reported and
    // the original value is retained, never mutated in place.
    assert_eq!(conflict, Some((b("key"), b("value2"))));
    assert_eq!(&*sl.get(&b("key")).unwrap(), b"value1");
}

#[test]
fn test_interleaved_inserts_order() {
    // Insert keys in interleaved order
    let sl = make_skiplist();

    // Insert in order: 0, 2, 4, 6, 8 then 1, 3, 5, 7, 9
    for i in (0..10).step_by(2) {
        let key = format!("{:03}", i);
        println!("Insert even: {}", key);
        sl.put(b(&key), b("even"));
    }

    println!("After even inserts, checking all keys:");
    for i in 0..10 {
        let key = format!("{:03}", i);
        let r = sl.get(&b(&key));
        println!(
            "  {}: {}",
            key,
            if r.is_some() { "found" } else { "MISSING" }
        );
    }

    for i in (1..10).step_by(2) {
        let key = format!("{:03}", i);
        println!("Insert odd: {}", key);
        sl.put(b(&key), b("odd"));
        // Check after each odd insert
        let r000 = sl.get(&b("000"));
        let r001 = sl.get(&b("001"));
        let r002 = sl.get(&b("002"));
        let r003 = sl.get(&b("003"));
        let r004 = sl.get(&b("004"));
        println!(
            "  After {}: 000={}, 001={}, 002={}, 003={}, 004={}",
            key,
            r000.is_some(),
            r001.is_some(),
            r002.is_some(),
            r003.is_some(),
            r004.is_some()
        );
    }

    // Verify all keys present
    println!("Final check:");
    for i in 0..10 {
        let key = format!("{:03}", i);
        let result = sl.get(&b(&key));
        if let Some(v) = &result {
            println!("Found {}: {:?}", key, String::from_utf8_lossy(v));
        } else {
            println!("MISSING: {}", key);
        }
        assert!(result.is_some(), "Key {} not found", key);
    }
}
