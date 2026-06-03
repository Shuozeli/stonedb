# Postmortem: SkipList Tests Failed to Catch Bugs

**Date:** 2026-04-04
**Author:** StoneDB Team
**Related PR:** SkipList implementation with pluggable KeyComparator

---

## Summary

Our SkipList implementation passed all unit tests but contained critical bugs that were exposed only after adding tests that should have existed from the beginning. Three new tests revealed that `lower_bound` uses raw byte comparison instead of the injected comparator, violating the KeyComparator abstraction.

---

## Bugs Exposed by New Tests

### Bug 1: `lower_bound` Bypasses Comparator

**Test:** `test_lower_bound_uses_comparator`
**Expected:** Searching for "key2" returns "value2"
**Actual:** Returns "value1"

**Root Cause:** The `lower_bound` method manually slices bytes (`key[..len-8]`) instead of using `cmp.compare_key()`. This works by accident for InternalKey (which has 8-byte suffix) but fails for BytewiseComparator.

```rust
// BUGGY CODE:
let next_user_key_len = next_node.key.len().saturating_sub(8);
let cmp_result = next_node.key[..next_user_key_len].cmp(&search_key[..search_user_key_len]);
```

**Should Be:**
```rust
let cmp_result = cmp.compare_key(&next_node.key, search_key);
```

---

### Bug 2: `lower_bound` Returns Wrong Result for Keys Beyond All Entries

**Test:** `test_lower_bound_at_end`
**Expected:** Searching for "z" when all keys are "a", "b", "c" returns None
**Actual:** Returns Some

**Root Cause:** When traversal reaches end of list (next_offset=0) and we have a prefix match but no exact match, the function incorrectly returns the last node instead of None.

---

### Bug 3: `lower_bound` Exact Match Returns Wrong Entry

**Test:** `test_lower_bound_exact_match`
**Expected:** Exact match for "key2" returns "value2"
**Actual:** Returns "value1"

**Root Cause:** Same as Bug 1 - raw byte comparison causes incorrect ordering.

---

## Why Our Original Tests Didn't Catch These Bugs

### 1. All Tests Used InternalKeyComparator

Our skiplist tests used `SkipList<BytewiseComparator>` (via `make_skl()`), but the *keys being tested* were simple byte strings like `b"key1"`, `b"key2"`. The `lower_bound` hardcoded 8-byte suffix assumption happened to work for these keys because:

- For `b"key1"[..]` (5 bytes), `len - 8 = -3` saturates to 0
- For `b"key2"[..]` (5 bytes), same
- `key[..0]` is empty slice, so `"" cmp ""` = Equal

**When User Keys Have Same Length:**
```rust
"key1"[..0] = ""  // empty
"key2"[..0] = ""  // empty
"" == ""  // true (accidentally works!)
```

This is a **coincidence**, not correct behavior!

### 2. No Test for Keys With Common Prefixes

We never tested keys like:
- `b"user:alice:data"` and `b"user:bob:data"`

With common prefixes, the hardcoded suffix logic breaks:
```rust
"user:alice:data"[..12] = "user:alice:"  // 12 chars
"user:bob:data"[..12]  = "user:bob:"     // 12 chars
"user:alice:" != "user:bob:"  // Wrong ordering!
```

### 3. No Test for lower_bound Edge Cases

Our tests only checked `get()` (exact match) and `contains()`. We never tested:
- `lower_bound` with key before first entry
- `lower_bound` with key after last entry
- `lower_bound` with key matching exact entry

### 4. Duplicate Detection Not Validated

We tested `put()` followed by `get()`, but never verified:
- List length after updates
- Multiple updates to same key
- Whether duplicates are actually prevented

### 5. Comparator Abstraction Violated

The KeyComparator trait was added to allow pluggable comparison, but:
- Tests only used BytewiseComparator
- No tests verified the abstraction was respected
- `lower_bound` was implemented with InternalKey assumptions baked in

---

## What Tests Should Have Existed

### Must-Have Tests for SkipList

```rust
// 1. Keys with common prefixes
#[test]
fn test_skiplist_common_prefixes() {
    let sl = make_skl();
    sl.put(b"user:alice:data", b"alice");
    sl.put(b"user:bob:data", b"bob");
    assert_eq!(sl.get(b"user:alice:data"), Some(b"alice"));
    assert_eq!(sl.get(b"user:bob:data"), Some(b"bob"));
}

// 2. lower_bound edge cases
#[test]
fn test_lower_bound_before_first() { /* ... */ }
#[test]
fn test_lower_bound_after_last() { /* ... */ }
#[test]
fn test_lower_bound_exact_match() { /* ... */ }

// 3. Comparator abstraction test
#[test]
fn test_lower_bound_uses_comparator() {
    // Use InternalKeyComparator to verify abstraction is respected
}

// 4. Duplicate prevention
#[test]
fn test_duplicate_not_inserted() {
    let sl = make_skl();
    sl.put(b"key", b"v1");
    sl.put(b"key", b"v2");
    assert_eq!(sl.len(), 1);  // Should NOT be 2!
}

// 5. Variable-length keys
#[test]
fn test_variable_length_keys() {
    let sl = make_skl();
    sl.put(b"a", b"1");
    sl.put(b"aa", b"2");
    sl.put(b"aaa", b"3");
    assert_eq!(sl.len(), 3);
}
```

---

## Lessons Learned

### 1. Test the Abstraction, Not Just One Implementation

When implementing a trait like `KeyComparator`:
- Test with ALL implementations
- Add specific tests that verify the abstraction is respected
- Don't rely on "accidental" correctness

### 2. Test Edge Cases From Day 1

- Empty list
- Single element
- Keys before/after all entries
- Keys with common prefixes
- Keys of varying lengths
- Duplicate insertions

### 3. Property-Based Testing

Consider adding property-based tests:
- "For any keys k1 < k2 < k3, lower_bound(k2) returns correct result"
- "List length = number of unique keys inserted"
- "All inserted keys are retrievable"

### 4. Don't Bake Assumptions Into Generic Code

The `lower_bound` method assumed keys have 8-byte suffixes because InternalKey does. This violated the KeyComparator abstraction.

**Rule:** Generic code must use the trait methods, not assume specific structure.

---

## Action Items

- [ ] Fix `lower_bound` to use `cmp.compare_key()` instead of raw byte slicing
- [ ] Add tests for all edge cases listed above
- [ ] Add property-based tests using `proptest`
- [ ] Add test that verifies KeyComparator abstraction is respected
- [ ] Review other methods for similar abstraction violations

---

## References

- RocksDB skiplist tests: `memtable/skiplist_test.cc`
- AgateDB skiplist: `skiplist/src/list.rs`
- KeyComparator trait design from AgateDB
