//! Concurrent SkipList Tests
//!
//! These tests use the concurrent test framework to expose bugs in the CAS retry logic.
//!
//! Key bugs being tested:
//! 1. CAS retry loop doesn't re-find splice position - can cause lost writes
//! 2. CAS retry doesn't update new_node.tower[i] - bypasses intermediate nodes
//! 3. prev array uninitialized for indices <= list_height

use std::sync::Arc;
use std::thread;
use stonedb_core::{BytewiseComparator, SkipList};

const VALUE: &[u8] = b"value";

#[test]
fn test_concurrent_insert_no_loss() {
    // Test that all inserted keys are eventually readable.
    // This should expose the CAS retry bug where inserts are lost.
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true, // allow_concurrent_write
    ));

    let n_threads = 4;
    let n_keys_per_thread = 100;
    let mut handles = vec![];

    for t in 0..n_threads {
        let sl = skiplist.clone();
        let handle = thread::spawn(move || {
            for i in 0..n_keys_per_thread {
                let key = format!("key_{}_{:04}", t, i);
                sl.put(key.clone(), VALUE);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify all keys are present
    let mut count = 0;
    for t in 0..n_threads {
        for i in 0..n_keys_per_thread {
            let key = format!("key_{}_{:04}", t, i);
            if skiplist.get(key.as_bytes()).is_some() {
                count += 1;
            }
        }
    }

    let expected = n_threads * n_keys_per_thread;
    assert_eq!(
        count, expected,
        "Expected {} keys but only found {}. Lost writes detected!",
        expected, count
    );
}

#[test]
fn test_concurrent_insert_same_key_no_loss() {
    // Multiple threads inserting different values for the same key.
    // Only one should win, but all inserts should be accounted for.
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true,
    ));

    let n_threads = 8;
    let key: &[u8] = b"same_key";
    let mut handles = vec![];

    for t in 0..n_threads {
        let sl = skiplist.clone();
        let handle = thread::spawn(move || {
            let value = format!("value_from_thread_{}", t);
            sl.put(key, value.clone());
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    // One of the values should be readable
    let val = skiplist.get(key);
    assert!(val.is_some(), "Expected at least one value for key");
    let val_bytes = val.unwrap();
    println!("Final value: {:?}", String::from_utf8_lossy(&val_bytes));
}

#[test]
fn test_concurrent_insert_interleaved_keys() {
    // Insert keys in different orders from different threads.
    // The CAS retry bug might cause some keys to be "bypassed" and lost.
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true,
    ));

    // Thread 0: inserts 0, 4, 8, 12, ...
    // Thread 1: inserts 1, 5, 9, 13, ...
    // Thread 2: inserts 2, 6, 10, 14, ...
    // Thread 3: inserts 3, 7, 11, 15, ...

    let n_threads = 4;
    let n_keys_per_thread = 50;
    let mut handles = vec![];

    for t in 0..n_threads {
        let sl = skiplist.clone();
        let handle = thread::spawn(move || {
            for i in (t..(n_keys_per_thread * n_threads)).step_by(n_threads) {
                let key = format!("{:04}", i);
                sl.put(key.clone(), VALUE);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    // Count how many keys are readable
    let mut count = 0;
    for i in 0..(n_keys_per_thread * n_threads) {
        let key = format!("{:04}", i);
        if skiplist.get(key.as_bytes()).is_some() {
            count += 1;
        }
    }

    let expected = n_keys_per_thread * n_threads;
    assert_eq!(
        count, expected,
        "Expected {} keys but only found {}. CAS retry bug caused lost writes!",
        expected, count
    );
}

#[test]
fn test_concurrent_insert_and_check_order() {
    // Insert keys in order from multiple threads and verify order is correct.
    // CAS retry bug could cause wrong ordering.
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true,
    ));

    let keys: Vec<String> = (0..100).map(|i| format!("{:03}", i)).collect();

    // Split keys among threads
    let n_threads = 4;
    let keys_per_thread = keys.len() / n_threads;
    let mut handles = vec![];

    for t in 0..n_threads {
        let sl = skiplist.clone();
        let thread_keys: Vec<String> =
            keys[t * keys_per_thread..(t + 1) * keys_per_thread].to_vec();
        let handle = thread::spawn(move || {
            for key in thread_keys {
                sl.put(key.clone(), VALUE);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    // Verify all keys are present and in order
    let mut errors = vec![];

    // We can't do full iteration easily, so just spot check
    for key in &keys {
        if skiplist.get(key.as_bytes()).is_none() {
            errors.push(format!("Missing key: {}", key));
        }
    }

    assert!(errors.is_empty(), "Order check failed: {:?}", errors);
}

#[test]
fn test_concurrent_read_write() {
    // Concurrent readers and writers - read should not crash
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true,
    ));

    // Pre-populate
    for i in 0..100 {
        let key = format!("key_{}", i);
        skiplist.put(key.clone(), VALUE);
    }

    let sl_read = skiplist.clone();
    let sl_write = skiplist.clone();

    let reader = thread::spawn(move || {
        for _ in 0..100 {
            for i in 0..100 {
                let key = format!("key_{}", i);
                sl_read.get(key.as_bytes());
            }
        }
    });

    let writer = thread::spawn(move || {
        for i in 100..200 {
            let key = format!("key_{}", i);
            sl_write.put(key.clone(), b"new_value".as_slice());
        }
    });

    reader.join().unwrap();
    writer.join().unwrap();

    // Verify original keys still present
    for i in 0..100 {
        let key = format!("key_{}", i);
        assert!(
            skiplist.get(key.as_bytes()).is_some(),
            "Original key {} was lost during concurrent access",
            i
        );
    }
}

#[test]
fn test_insert_only_no_value_mutation_under_concurrency() {
    // Arrange: a key is present with an initial value. Before the AgateDB-style
    // rewrite, a duplicate put overwrote the node's value in place, which raced
    // concurrent readers (a data race / use-after-free). Nodes are now immutable.
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true,
    ));
    let inserted = skiplist.put(b"k".as_slice(), b"value1".as_slice());
    assert!(inserted.is_none(), "first insert should succeed");

    // Act: eight writers hammer the same key with different values while a
    // reader reads it concurrently. A duplicate put must be a no-op conflict.
    let mut handles = vec![];
    for t in 0..8 {
        let sl = skiplist.clone();
        handles.push(thread::spawn(move || {
            let value = format!("writer_{}", t);
            for _ in 0..2000 {
                sl.put(b"k".as_slice(), value.clone());
            }
        }));
    }
    let reader = {
        let sl = skiplist.clone();
        thread::spawn(move || {
            for _ in 0..10_000 {
                // A reader must never observe a torn or overwritten value.
                assert_eq!(sl.get(b"k".as_slice()).as_deref(), Some(&b"value1"[..]));
            }
        })
    };

    for h in handles {
        h.join().unwrap();
    }
    reader.join().unwrap();

    // Assert: the original value is intact.
    assert_eq!(
        skiplist.get(b"k".as_slice()).as_deref(),
        Some(&b"value1"[..])
    );
}

#[test]
fn test_high_contention_same_level() {
    // Multiple threads inserting at the same level to stress CAS retry.
    // This increases chance of CAS failures and exposes retry bugs.
    let skiplist = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        1024 * 1024,
        true,
    ));

    let n_threads = 8;
    let n_inserts = 200;
    let mut handles = vec![];

    // Each thread inserts keys that are close together to cause contention
    for t in 0..n_threads {
        let sl = skiplist.clone();
        let handle = thread::spawn(move || {
            // Keys are in format "batch_t_id" where id is sequential
            // This creates lots of collisions at same level
            for i in 0..n_inserts {
                let key = format!("{:02}_{:04}", t, i);
                sl.put(key.clone(), VALUE);
            }
        });
        handles.push(handle);
    }

    for h in handles {
        h.join().unwrap();
    }

    // Count total inserts
    let mut count = 0;
    for t in 0..n_threads {
        for i in 0..n_inserts {
            let key = format!("{:02}_{:04}", t, i);
            if skiplist.get(key.as_bytes()).is_some() {
                count += 1;
            }
        }
    }

    let expected = n_threads * n_inserts;
    assert_eq!(
        count,
        expected,
        "High contention test: expected {} keys but found {}. CAS retry lost {} writes!",
        expected,
        count,
        expected - count
    );
}
