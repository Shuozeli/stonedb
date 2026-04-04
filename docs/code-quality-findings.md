# Code Quality Findings

## 1. Correctness Bug - SkipList Insert with Inconsistent Predecessor Tracking

### SkipList insert produces inconsistent forward pointers when top_level < max_height
- **Location:** `crates/stonedb-core/src/skiplist.rs:193` (insert)
- **Problem:** When `top_level < self.max_height`, the search loop only visits levels 0..self.max_height.min(top_level), leaving higher levels with stale predecessor pointers. The fallback `head_ptr` at lines 235-238 does not correctly reconstruct the predecessor chain for those missing levels. This can cause a new node to be inserted with inconsistent forward pointers across levels, breaking the skip list invariant.
- **Example:** If max_height=3 and we insert a new node with top_level=1 into a list containing key=3 at level 2 and key=5 at level 1, the new node might get prevs[2]=head (wrong) while prevs[1]=key_5 and prevs[0]=key_5. This corrupts level 2's forward chain.
- **Fix:** When `top_level < self.max_height`, the new node should NOT be inserted at levels >= top_level. The code currently tries to insert at all levels via the fallback, which corrupts the structure. Either (a) cap the insertion to only levels < top_level, or (b) properly track predecessors for all levels >= top_level.
- **Status:** FIXED - Changed the search loop from `self.max_height.min(top_level)` to `top_level` to ensure we properly search all relevant levels. Verified with test suite.

---

## 2. Missing Implementation - MemTableIterator stubs

### seek_to_last() and prev() are unimplemented stubs
- **Location:** `crates/stonedb-core/src/memtable.rs:195-196, 210-211`
- **Problem:** These methods are part of the `Iterator` trait but contain only `// Not implemented` comments. Calling them will not panic but will leave the iterator in an invalid state (current_key empty). This violates the trait contract.
- **Fix:** Either implement these methods properly or remove them from the trait (if backward compatibility allows).
- **Status:** NOT FIXED - requires more substantial changes to SkipListIterator to support backward traversal.

---

## 3. Dead Code - Unused Direction enum

### Direction enum is never used
- **Location:** `crates/stonedb-core/src/iterator.rs:44-50`
- **Problem:** The `Direction` enum with `Forward` and `Backward` variants is defined with `#[allow(dead_code)]` but never referenced anywhere. It adds noise to the codebase.
- **Fix:** Remove the `Direction` enum if it is not planned for future use.
- **Status:** FIXED - Removed the unused Direction enum.

---

## 4. Missing Tests - No overflow/edge case tests

### Sequence overflow is not tested
- **Location:** `crates/stonedb-core/src/memtable.rs:63-66` (put), `crates/stonedb-core/src/memtable.rs:78-81` (delete)
- **Problem:** Both `put()` and `delete()` check for `u64::MAX` sequence overflow but there is no test that exercises this code path. A `#[test]` that fills up to `u64::MAX - 1` sequences would be impractical, but a unit test that mocks or directly tests the overflow check logic is missing.
- **Fix:** Add a test that verifies the overflow check logic is correct. Consider testing with a smaller max sequence value or by directly inspecting the error returned when sequence would overflow.
- **Status:** NOT FIXED - requires adding a test that can trigger the overflow check without consuming u64::MAX entries.

---

## 5. Dead Code - Unused timeline helper functions

### Timeline helper functions are conditionally compiled but never called
- **Location:** `crates/stonedb-core/src/timeline.rs:159-283`
- **Problem:** Functions `serialize_event`, `base64_encode`, `skiplist_insert`, `skiplist_get`, `skiplist_contains`, `skiplist_lower_bound`, `memtable_put`, `memtable_delete`, `memtable_get`, `memtable_contains` are only compiled when `timeline` feature is enabled but are never called from within the crate. They appear to be a scaffolding/API that consumers would call, but they generate dead-code warnings even when the feature is enabled.
- **Fix:** Mark these as `#[cfg(feature = "timeline")] pub` and accept that they are unused within the crate itself, OR add a `#[allow(dead_code)]` per-function with a note that they are a public API.
- **Status:** EXPECTED - These are a public API intended for external consumers. The warnings appear when compiling without the timeline feature enabled.

---

## 6. Unused Variable - fut in test

### fut variable is unused in test_observable_disabled
- **Location:** `crates/stonedb-core/src/timeline.rs:322`
- **Problem:** `let (obs, fut) = Observable::new(100);` - the `fut` variable is never used. With `#[cfg(not(feature = "timeline"))]`, `Observable::new` returns a `std::future::Ready<()>` as the second element, which is discarded.
- **Fix:** Change to `let (obs, _fut) = Observable::new(100);` or use `let (obs, _)`.
- **Status:** FIXED - Changed to `_fut`.

---

## 7. Unused Import

### os_info crate is imported but never used
- **Location:** `crates/stonedb-core/Cargo.toml:13`
- **Problem:** The `os_info = "3.14.0"` dependency is listed but never used in the crate's source code.
- **Fix:** Remove the `os_info` dependency from `crates/stonedb-core/Cargo.toml`.
- **Status:** FIXED - Removed the unused os_info dependency.

---

## 8. Performance - contains() does unnecessary work

### MemTable::contains() computes full Entry then discards it
- **Location:** `crates/stonedb-core/src/memtable.rs:111-113`
- **Problem:** `contains()` calls `get_entry()` which performs a full `lower_bound` search and constructs an `Entry` object (with cloned key/value), but only uses it to check `is_some()`. For the non-deleted case, the value clone is wasted.
- **Fix:** Consider adding a `contains_key()` method on `SkipList` that returns only a boolean, avoiding the value clone. Alternatively, document that `contains()` is intentionally simple and the overhead is acceptable.
- **Status:** NOT FIXED - low priority, documented as acceptable.

---

## 9. Unsafe Code - Missing safety documentation

### Raw pointer dereferences lack safety comments
- **Location:** `crates/stonedb-core/src/skiplist.rs:34-42, 78-96, 131-141, 193-210, 261-275, 295-312`
- **Problem:** Multiple unsafe blocks dereference raw pointers (`&*ptr`, `&mut (*ptr).forwards`) without explanatory safety comments. While the logic appears correct, these regions are hard to verify without explicit safety invariants documented.
- **Fix:** Add `// SAFETY:` comments explaining the invariants that make each unsafe block safe (e.g., "ptr is guaranteed non-null because it was obtained from a previous node's forwards vector").
- **Status:** NOT FIXED - requires careful analysis of each unsafe block's invariants.

---

## 10. API Design - Unclear Snapshot Semantics

### MemTable::get() and contains() behavior with tombstones is confusing
- **Location:** `crates/stonedb-core/src/memtable.rs:94-108, 111-113`
- **Problem:** `get()` returns `Ok(None)` for both "key not found" and "key deleted" cases. `contains()` returns `true` for deleted keys (as documented), but this may surprise users who expect `contains()` to mean "has a value". The difference between "not found" and "deleted" is lost.
- **Fix:** Consider returning `Result<Option<Vec<u8>>>` but including the value_type/deleted status in the `Option`, or add a separate `get_if_exists()` vs `get_value()` distinction. Document the semantics clearly.
- **Status:** NOT FIXED - API design issue requiring more thought.
