# Iterator Benchmark Lesson

**Date:** 2026-04-05
**Topic:** SkipList forward iteration performance investigation

---

## The Problem

Our forward iteration benchmark was showing **2.1ms** for 10K keys, which seemed 150x slower than AgateDB's 13µs.

Initial hypothesis: Our skiplist implementation was slow.

---

## The Truth

The skiplist was **always capable of O(n) forward iteration** - the level-0 linked list provides this for free.

Our benchmark was the problem:

```rust
// BAD: O(n log n) - get() does a full tower traversal each time
for i in 0..10000 {
    let key = format!("{:016}", i);
    sl.get(key.as_bytes());  // ~log(n) comparisons per call
}
```

After fixing to use proper iterator:

```rust
// GOOD: O(n) - just follow level-0 linked list pointers
let mut iter = sl.iter_ref();
iter.seek_to_first();
while iter.valid() {
    iter.next();
}
```

Result: **2.1ms → 42µs** (50x improvement)

---

## Key Lessons

### 1. Benchmark What You Actually Want to Measure

If you want to measure iteration performance, **use iteration**, not repeated search.

- `get()` in loop = measures search performance × iteration count
- `iter.next()` = measures actual traversal performance

### 2. Don't Confuse O(n log n) with O(n)

```
get() in loop:     O(n log n) where n = number of iterations
                    = 10K × log(10K) ≈ 130K operations

iter.next():       O(n) where n = number of iterations
                    = 10K simple pointer reads
```

### 3. Skiplist Has Two Traversal Paths

| Path | Complexity | Use Case |
|------|------------|----------|
| Tower (levels 1-MAX_HEIGHT) | O(log n) | Search (`get()`) |
| Level-0 linked list | O(n) | Iteration |

The skiplist is optimized for search, but provides O(n) iteration via the bottom-level linked list.

### 4. Implementation Was Already There

The level-0 forward traversal was always possible - we just needed:
- `head.next_offset(0)` to get first node
- `node.next_offset(0)` to get next node

We only needed to implement `IterRef` to make this ergonomic.

---

## Why AgateDB Was 3x Faster Even After Fix

After using proper iteration, our 42µs vs AgateDB's 15µs still has a gap:

| Factor | StoneDB | AgateDB |
|--------|---------|---------|
| Variable-size nodes | No (full-size) | Yes (saves memory) |
| Arena impl | Different | Different |
| Memory layout | 240 bytes/node | ~200 bytes/node |

The gap is now likely due to memory efficiency, not algorithmic issues.

---

## Action Items

- [x] Implement IterRef for ergonomic iteration (DONE)
- [ ] Variable-size node allocation (saves memory, improves cache)
- [ ] Document O(n) vs O(log n) distinction in code comments

---

## Related

- Benchmark file: `crates/stonedb-core/benches/bench.rs`
- IterRef impl: `crates/stonedb-core/src/skiplist/mod.rs`
- Original comparison: AgateDB `skiplist/benches/bench_comparison.rs`
