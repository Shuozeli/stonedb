# Postmortem: SkipList CAS Retry Bug and Concurrent Write Loss

**Date:** 2026-04-05
**Author:** StoneDB Team
**Related PR:** Rewrite SkipList with Arena allocator (AgateDB-style)

---

## Summary

SkipList concurrent tests revealed that our implementation lost writes under concurrent access. Investigation showed two critical bugs in the CAS (Compare-And-Swap) retry logic that caused:
- Lost writes (expected 400 keys, found 396-399)
- Incorrect ordering under high contention

The root cause was that our CAS retry logic did not re-search for the correct splice position after a CAS failure, unlike the reference AgateDB implementation.

---

## Bugs Found

### Bug 1: CAS Retry Did Not Re-search Splice Position

**Test:** `test_concurrent_insert_no_loss`
**Expected:** 400 keys inserted across 4 threads
**Actual:** 396-399 keys found (1-4 lost writes)

**Root Cause:**

Our CAS retry loop simply retried with the same `next_offset`:

```rust
Err(actual) => {
    current = actual;  // BUG: Just retry with wrong next_offset!
}
```

AgateDB's correct implementation re-searches for the correct splice position:

```rust
Err(_) => {
    let (p, n) = unsafe { self.find_splice_for_level(&x.key, prev[i], i) };
    if p == n {
        // Duplicate key handling
    }
    prev[i] = p;  // Update prev
    next[i] = n;  // Update next - CRITICAL!
}
```

When a CAS fails, it means another thread modified the linked list. The `next_offset` we captured earlier is now stale. We must re-search to find the current successor node.

---

### Bug 2: `prev[i] == 0` Not Properly Handled in Concurrent Mode

**Test:** `test_high_contention_same_level`
**Expected:** 1600 keys across 8 threads
**Actual:** 1597 found (3 lost writes)

**Root Cause:**

When `prev[i]` is 0 (uninitialized), our code only set `prev[i] = head`:

```rust
if prev[i] == 0 {
    prev[i] = self.head_offset();
    // BUG: Did not update next[i]!
    let next_offset = next[i];  // next[i] was stale
}
```

AgateDB correctly searches for both:

```rust
if prev[i].is_null() {
    let (p, n) = unsafe {
        self.find_splice_for_level(&x.key, self.inner.head.as_ptr(), i)
    };
    prev[i] = p;  // Correct prev
    next[i] = n;  // Correct next - CRITICAL!
}
```

---

### Bug 3: Arena Used `Cell` (Not Thread-Safe)

**Symptom:** `Cell<*mut u8>` cannot be shared between threads

**Root Cause:**

Our Arena used `std::cell::Cell` for interior mutability, which is not `Send` or `Sync`:

```rust
// BROKEN:
pub struct Arena {
    start: *mut u8,
    ptr: Cell<*mut u8>,  // Cell is not thread-safe!
    capacity: usize,
}
```

**Fix:** Use atomic operations for allocation:

```rust
// FIXED:
pub struct Arena {
    start: *mut u8,
    ptr: AtomicPtr<u8>,  // Thread-safe via compare_exchange
    capacity: usize,
}
```

---

## Why Tests Passed Before

### 1. Single-Threaded Tests Never Triggered CAS Retry

Our `skiplist_basic_test` and `skiplist_debug_test` ran in single-threaded mode (`allow_concurrent_write = false`). The buggy CAS retry code was never executed.

### 2. Concurrent Tests Were Added But Had Wrong Signature

Concurrent tests were written for a different API (with `BytewiseComparator` parameter) that didn't match our new implementation:

```rust
// OLD TEST (didn't compile):
SkipList::with_capacity(BytewiseComparator::new(), 1024 * 1024, true)

// NEW API:
SkipList::with_capacity(1024 * 1024, true)
```

Tests were broken until we fixed the API mismatch.

### 3. No "Lost Write" Detection

Even after fixing compilation, no test explicitly checked that ALL inserted keys were retrievable. The `test_concurrent_insert_no_loss` test added this check and exposed the bug.

---

## Verification: AgateDB Does NOT Have This Bug

We verified by running AgateDB's own concurrent tests:

```bash
cd db/agatedb
cargo test -p skiplist
# All 8 tests PASSED including test_concurrent_basic_big_value
```

This confirms the bug is in our port, not in the reference implementation.

---

## The Fix

### CAS Retry Logic

```rust
// Concurrent mode: properly handle CAS failure
loop {
    if prev[i] == 0 {
        prev[i] = self.head_offset();
        let (p, n) = self.find_splice_for_level(
            &new_node.key, prev[i], i, arena,
        );
        prev[i] = p;
        next[i] = n;
    }

    let next_offset = next[i];
    new_node.set_next(i, next_offset);

    let prev_node = &mut *Self::get_node(arena, prev[i]);
    match prev_node.tower[i].compare_exchange(
        next_offset, node_offset, AtomicOrdering::SeqCst, AtomicOrdering::SeqCst
    ) {
        Ok(_) => break,
        Err(_) => {
            // CAS failed: re-search for correct splice position
            let (p, n) = self.find_splice_for_level(
                &new_node.key, prev[i], i, arena,
            );
            if p == n {
                // Duplicate key handling...
            }
            prev[i] = p;
            next[i] = n;
        }
    }
}
```

### Arena Thread Safety

```rust
pub fn alloc(&self, size: usize) -> usize {
    loop {
        let current_ptr = self.ptr.load(AtomicOrdering::SeqCst);
        let offset = /* calculate */;
        let new_ptr = /* calculate */;

        match self.ptr.compare_exchange(
            current_ptr, new_ptr, AtomicOrdering::SeqCst, AtomicOrdering::SeqCst
        ) {
            Ok(_) => return offset,
            Err(_) => continue,  // Another thread modified, retry
        }
    }
}
```

---

## Lessons Learned

### 1. Always Test Concurrent Code Concurrently

Single-threaded tests do not exercise concurrent code paths. We need:
- Concurrent stress tests
- Tests that verify no data loss under contention
- Tests that check ordering guarantees

### 2. Study Reference Implementation Before "Improving"

We had the AgateDB reference but our CAS retry logic diverged. Always verify against reference when implementing concurrent algorithms.

### 3. Thread-Safety Requires Explicit Design

Interior mutability via `Cell` works for single-threaded cases. For multi-threaded use, must use:
- `AtomicUsize`, `AtomicPtr`, etc. for simple cases
- `Mutex`, `RwLock` for complex cases
- `compare_exchange` loops for lock-free algorithms

### 4. Every Concurrent Test Must Check for Lost Writes

```rust
// MUST verify all keys are present:
for key in all_expected_keys {
    assert!(skiplist.get(key).is_some(), "Key {:?} was lost!", key);
}
```

---

## Action Items

- [x] Fix CAS retry to re-search splice position
- [x] Fix prev[i]==0 handling in concurrent mode
- [x] Fix Arena to use AtomicPtr instead of Cell
- [x] Verify concurrent tests pass against AgateDB reference
- [ ] Add more concurrent stress tests
- [ ] Add test that verifies ordering under concurrent writes
- [ ] Add property-based concurrent tests

---

## References

- AgateDB skiplist: `db/agatedb/skiplist/src/list.rs`
- Rust atomic ordering: [std::sync::atomic](https://doc.rust-lang.org/std/sync/atomic/)
- Lock-free CAS pattern: [compare_exchange](https://doc.rust-lang.org/std/sync/atomic/struct.AtomicUsize.html#method.compare_exchange)
