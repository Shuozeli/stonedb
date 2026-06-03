# SkipList Improvements Roadmap

**Date:** 2026-04-05
**Based on:** Comparative analysis with AgateDB skiplist

---

## Priority 1 - Critical Missing Features

### [x] 1. Add KeyComparator trait
**Impact:** HIGH
**Issue:** Hardcoded byte comparison, can't use custom key ordering

Created `crates/stonedb-core/src/skiplist/key.rs`:
- `KeyComparator` trait with `compare_key()` and `same_key()` methods
- `BytewiseComparator` - compares keys as raw bytes
- `FixedLengthSuffixComparator` - ignores fixed-length suffix for `same_key`

Made `SkipList<C>` generic over `C: KeyComparator`.

**Status:** Done (2026-04-05)

---

### [ ] 2. Add Iterator (IterRef)
**Impact:** HIGH
**Issue:** No way to iterate the skiplist

Add `IterRef` struct with methods:
- `valid()`, `key()`, `value()`
- `next()`, `prev()`
- `seek()`, `seek_to_first()`, `seek_to_last()`
- `seek_for_prev()`

**Status:** Not started

---

### [ ] 3. Add find_near() method
**Impact:** HIGH
**Issue:** Can't do range queries efficiently

Implement:
```rust
unsafe fn find_near(&self, key: &[u8], less: bool, allow_equal: bool) -> *const Node
```

Supports:
- Find rightmost node where `node.key < key`
- Find leftmost node where `node.key > key`
- Exact match handling

**Status:** Not started

---

## Priority 2 - Memory Optimization

### [ ] 4. Variable-size node allocation
**Impact:** MEDIUM
**Issue:** Wastes memory by allocating full `size_of::<Node>` for every node

Change node allocation to match AgateDB:
```rust
let not_used = (MAX_HEIGHT - height - 1) * mem::size_of::<AtomicUsize>();
let node_offset = arena.alloc(size - not_used);
```

**Status:** Not started

---

### [ ] 5. Arena auto-growth
**Impact:** MEDIUM
**Issue:** Panics on OOM instead of growing

Implement arena growth similar to AgateDB:
```rust
if offset + size > self.capacity {
    // Alloc new buf and copy data
}
```

**Status:** Not started

---

## Priority 3 - Bug Fixes

### [ ] 6. Implement Drop for SkiplistInner
**Impact:** HIGH
**Issue:** Memory leak - nodes not dropped when Skiplist is dropped

Add proper Drop implementation that traverses and drops all nodes.

**Status:** Not started

---

### [ ] 7. Add mem_size() method
**Impact:** LOW
```rust
pub fn mem_size(&self) -> usize {
    self.inner.arena.len()
}
```

**Status:** Not started

---

### [ ] 8. Remove dead code in get_with_key()
**Impact:** LOW
**Issue:** Lines 396-402 contain dead code (commented-out unreachable path)

**Status:** Not started

---

## Priority 4 - Test Coverage

### [ ] 9. Add comprehensive tests
**Impact:** MEDIUM
**Issue:** Minimal test coverage

Add AgateDB-style test cases:
- 20+ test cases for `find_near`
- Concurrent iterator tests
- Edge cases with various key patterns

**Status:** Not started

---

## Priority 5 - Minor Improvements

### [ ] 10. Add AsRef trait implementation
**Impact:** LOW
```rust
impl<C> AsRef<Skiplist<C>> for Skiplist<C>
```

**Status:** Not started

---

## Progress Log

| Date | Item | Status |
|------|------|--------|
| 2026-04-05 | Initial document created | Done |
