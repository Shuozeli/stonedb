# SkipList Bug Postmortem

**Date:** 2026-04-04
**Reviewer:** Claude Code (sub-agent + AgateDB comparison)
**Status:** Identified, not yet fixed

---

## Executive Summary

Concurrent SkipList implementation has **5 critical bugs** that prevent correct operation. Three bugs cause data loss in concurrent scenarios, one causes undefined behavior in single-threaded scenarios, and one prevents concurrent access entirely.

The bugs were identified by comparing against the reference implementation in [AgateDB](https://github.com/tikv/agatedb/blob/master/skiplist/src/list.rs).

---

## Reference: AgateDB vs StoneDB Comparison

| Aspect | AgateDB (Correct) | StoneDB (Buggy) |
|--------|-------------------|-----------------|
| **CAS retry re-find splice** | Yes, calls `find_splice_for_level` | No, only retries CAS on same location |
| **Tower pointer update on retry** | Yes, inside retry loop | No, computed once before loop |
| **prev[] initialization** | `prev[list_height+1] = head` | `prev[i>list_height] = head` (all) |
| **Duplicate in CAS retry** | Yes, properly handled | No, missing entirely |
| **Inner pointer type** | `Arc<SkiplistInner>` | `Rc<RefCell<Arena>>` |

---

## Bugs Found

### Bug 1: `Rc` Instead of `Arc` - Cannot Share Across Threads

**Severity:** Critical
**Location:** `crates/stonedb-core/src/skiplist/mod.rs:185`

```rust
pub struct SkipList<C> {
    inner: std::rc::Rc<SkiplistInner<C>>,  // ❌ Rc is not Send!
    allow_concurrent_write: bool,
}
```

**Problem:** `std::rc::Rc` is not `Send` or `Sync`, so `SkipList` cannot be shared across threads even when `allow_concurrent_write = true`.

**Impact:** Concurrent tests fail to compile with:
```
`Rc<SkiplistInner<BytewiseComparator>>` cannot be sent between threads safely
```

**Fix:** Change `Rc` to `Arc`.

---

### Bug 2: CAS Retry Loop Doesn't Re-find Splice Position

**Severity:** Critical
**Location:** `crates/stonedb-core/src/skiplist/mod.rs:330-350`

```rust
loop {
    match prev_node.tower[i].compare_exchange(
        current,
        node_offset,
        SeqCst,
        SeqCst,
    ) {
        Ok(_) => break,
        Err(actual) => {
            // ❌ Only updates expected value, doesn't re-find splice!
            current = actual;
        }
    }
}
```

**Problem:** When CAS fails, the code only updates `current` (the expected value) but doesn't re-find the correct splice position. If another thread inserted a node between `prev` and `next`, we retry with the wrong position.

**Impact:** Lost writes - nodes get bypassed and never reachable from the list.

**Fix:** On CAS failure, must re-call `find_splice_for_level` to find the new correct position.

---

### Bug 3: CAS Retry Doesn't Update `new_node.tower[i]`

**Severity:** Critical
**Location:** `crates/stonedb-core/src/skiplist/mod.rs:327-328`

```rust
// Set new node's next first (store-only, no CAS needed)
new_node.set_next(i, next_offset);

// CAS to link prev[i] -> new_node
// ...
loop {
    match prev_node.tower[i].compare_exchange(current, node_offset, ...) {
        // ❌ If CAS fails and retries, new_node.tower[i] still points to old next_offset!
        Err(actual) => current = actual,
    }
}
```

**Problem:** `new_node.tower[i]` is set once before the CAS loop. If CAS fails and retries, `new_node.tower[i]` still points to the original `next_offset`, but the CAS is trying to link to a different position.

**Impact:** Even if CAS "succeeds", the list structure is broken - `new_node` still points to old successor, bypassing any intermediate nodes.

**Fix:** On CAS failure, must update `new_node.tower[i]` with the new `next_offset` before retrying.

---

### Bug 4: `prev` Array Uninitialized for Indices ≤ `list_height`

**Severity:** Critical
**Location:** `crates/stonedb-core/src/skiplist/mod.rs:260-271`

```rust
let mut prev = [0usize; MAX_HEIGHT];
let mut next = [0usize; MAX_HEIGHT];

// ❌ Only initializes indices > list_height
for i in (list_height + 1)..MAX_HEIGHT {
    prev[i] = self.head_offset();
}

// Search from top down
for i in (0..=list_height).rev() {
    let (p, n) = unsafe {
        // ❌ When i = list_height - 1, accesses prev[list_height] which is UNINITIALIZED!
        self.find_splice_for_level(&key, prev[i + 1], i, &arena, cmp)
    };
    prev[i] = p;
    next[i] = n;
}
```

**Problem:** `prev` array is only initialized for indices > `list_height`. But the loop accesses `prev[i + 1]` when `i` goes from `list_height` down to 0. When `i = list_height - 1`, it reads `prev[list_height]` which was never initialized.

**Impact:** Undefined behavior - reading uninitialized memory. Could cause crashes or incorrect behavior.

**Fix:** Initialize `prev` properly before the loop, or initialize `prev[0..=list_height]` as well.

---

### Bug 5: Duplicate Key Returns Old Value Instead of Updating

**Severity:** Critical
**Location:** `crates/stonedb-core/src/skiplist/mod.rs:275-284`

```rust
// Check if key already exists
if p == n && p != 0 {
    unsafe {
        let node = &*Self::get_node(&arena, p);
        if node.value != value {
            return Some((key, value));  // ❌ Returns conflict, doesn't update!
        }
    }
    return None; // Same value, no-op
}
```

**Problem:** When a key already exists with a different value, the code returns `Some((key, value))` (conflict) instead of updating the existing node's value.

**Impact:** Test `test_update_same_key` fails:
```
Insert "key" -> "value1" ✓
Insert "key" -> "value2" (second insert with different value)
Get "key" returns "value1" (old value, not updated!)
```

**Expected:** Second insert should update the value to "value2".
**Actual:** Returns conflict indicator and keeps old value.

**Fix:** On duplicate key with different value, should update the existing node's value, not return conflict.

---

## Test Results

### Basic Tests (Single-threaded mode)

| Test | Result | Bug |
|------|--------|-----|
| `test_simple_insert_and_get` | ✅ PASS | - |
| `test_two_inserts` | ✅ PASS | - |
| `test_sequential_keys` | ✅ PASS | - |
| `test_update_same_key` | ❌ FAIL | Bug 5 |

### Concurrent Tests (Cannot compile)

```
error: `Rc<SkiplistInner<BytewiseComparator>>` cannot be sent between threads safely
```

Concurrent tests cannot even compile due to Bug 1.

---

## Root Cause Analysis

The implementation appears to be a simplified version of AgateDB's SkipList, but the CAS retry logic was not correctly adapted. Key simplifications that introduced bugs:

1. **No re-finding of splice position on CAS failure** - AgateDB re-searches after each CAS failure
2. **No update of new node's tower on retry** - The new node's next pointer must be updated to match the new splice position
3. **Incorrect prev array initialization** - Logic doesn't match the usage pattern in the search loop
4. **Missing update semantics for duplicate keys** - Returns conflict instead of performing update

---

## Reference: AgateDB Correct Implementation

AgateDB's CAS retry logic:
1. On CAS failure, re-call `find_splice_for_level` to find NEW correct position
2. Update `prev[i]` and `next[i]` with the new positions
3. Update `new_node.tower[i]` with the new `next_offset`
4. Retry CAS with correct expected value

### AgateDB CAS Retry (Correct)

```rust
for i in 0..=height {
    if self.allow_concurrent_write {
        loop {
            if prev[i].is_null() {
                // Re-find splice position
                let (p, n) = unsafe {
                    self.find_splice_for_level(&x.key, self.inner.head.as_ptr(), i)
                };
                prev[i] = p;
                next[i] = n;
            }
            let next_offset = self.inner.arena.offset(next[i]);
            // ✅ Updates tower INSIDE loop, every iteration
            x.tower[i].store(next_offset, Ordering::SeqCst);

            match unsafe { &*prev[i] }.tower[i].compare_exchange(...) {
                Ok(_) => break,
                Err(_) => {
                    // ✅ Re-finds splice on CAS failure
                    let (p, n) = unsafe { self.find_splice_for_level(&x.key, prev[i], i) };
                    if p == n {
                        // ✅ Handles duplicate in CAS retry path
                        // ...
                    }
                    prev[i] = p;
                    next[i] = n;
                    // ✅ Loop retries with correct prev/next/tower
                }
            }
        }
    }
}
```

### StoneDB CAS Retry (BUGGY)

```rust
for i in 0..=height {
    // ❌ next_offset computed ONCE before CAS loop
    let next_offset = Self::get_node(&arena, next[i]).next_offset(i);

    // ❌ Tower set ONCE before CAS loop
    new_node.set_next(i, next_offset);

    if self.allow_concurrent_write {
        let mut current = next_offset;
        loop {
            match prev_node.tower[i].compare_exchange(current, node_offset, ...) {
                Ok(_) => break,
                Err(actual) => {
                    // ❌ Only updates current, doesn't re-find splice!
                    current = actual;
                }
            }
        }
    }
}
```

**The Critical Difference**: When CAS fails, AgateDB re-finds the splice position and updates `x.tower[i]` inside the loop. StoneDB does neither.

---

## Memory Model: Arena Choice

### AgateDB Memory Model

AgateDB uses a simpler, more direct memory model:

```rust
struct SkiplistInner {
    height: AtomicUsize,
    head: NonNull<Node>,           // Raw pointer, thread-safe
    arena: Arena,                  // Direct ownership, not wrapped
    c: C,
}

pub struct SkipList<C> {
    inner: Arc<SkiplistInner>,    // Arc for thread-safe sharing
    c: C,
    allow_concurrent_write: bool,
}
```

- `Arena` is used directly without `RefCell` wrapping
- `NonNull<Node>` for head pointer - explicit nullability
- `Arc<SkiplistInner>` allows safe sharing across threads

### StoneDB Memory Model

StoneDB uses a more wrapped/encapsulated model:

```rust
struct SkiplistInner<C> {
    height: AtomicUsize,
    head: usize,                                     // Offset, not pointer
    arena: std::rc::Rc<std::cell::RefCell<Arena>>, // Wrapped twice!
    c: C,
}

pub struct SkipList<C> {
    inner: std::rc::Rc<SkiplistInner<C>>,          // Rc, not Arc
    allow_concurrent_write: bool,
}
```

- `Arena` wrapped in `Rc<RefCell<Arena>>` - borrow-checked at runtime, not compile-time
- `usize` offset instead of pointer - requires arena reference to dereference
- `Rc` instead of `Arc` - cannot be shared across threads

### Why This Matters

| Aspect | AgateDB | StoneDB |
|--------|---------|---------|
| Thread safety | Compile-time (`Arc`) | None (`Rc`) |
| Borrow checking | Compile-time | Runtime (`RefCell`) |
| Dereference complexity | Direct pointer | Offset + arena |
| Concurrent writes | Safe with `Arc` | Unsafe even with `allow_concurrent_write=true` |

---

## Next Steps

1. Fix Bug 1: Change `Rc` to `Arc`
2. Fix Bug 4: Properly initialize `prev` array
3. Fix Bug 5: Implement update semantics for duplicate keys
4. Fix Bugs 2 & 3: Implement correct CAS retry with re-finding splice position
5. Re-enable concurrent tests after Bug 1 is fixed
6. Run stress tests to verify fixes

---

## Files Modified

- `crates/stonedb-core/src/skiplist/mod.rs` - Main implementation (bugs located here)
- `crates/stonedb-core/src/lib.rs` - Added `BytewiseComparator` export
- `crates/stonedb-core/tests/skiplist_debug_test.rs` - New debug tests
- `crates/stonedb-core/tests/skiplist_basic_test.rs` - New basic tests (incomplete, blocked on Bytes lifetime issues)
- `crates/stonedb-core/tests/skiplist_concurrent_test.rs` - New concurrent tests (cannot compile due to Bug 1)
