# AgateDB SkipList Study

<!-- agent-updated: 2026-04-04T04:00:00Z -->

## Overview

Studied TiKV/agatedb SkipList implementation to understand production-grade LSM-tree in-memory structure.

**Reference**: [AgateDB skiplist/src/list.rs](https://github.com/tikv/agatedb/blob/master/skiplist/src/list.rs)

## Key Techniques

### 1. Arena Allocator

**Problem with Box<Node>**: Each insert calls malloc, causing heap fragmentation and allocation overhead.

**Solution**: Pre-allocate a large memory arena, place nodes at byte offsets.

```rust
#[repr(C)]  // C layout for predictable offsets
pub struct Node {
    key: Bytes,
    value: Bytes,
    height: usize,
    prev: AtomicUsize,                     // Offset to previous node
    tower: [AtomicUsize; MAX_HEIGHT],     // Forward pointers as atomic offsets
}

fn alloc(arena: &Arena, key: Bytes, value: Bytes, height: usize) -> usize {
    // Allocate only what's needed for this height
    let not_used = (MAX_HEIGHT - height - 1) * mem::size_of::<AtomicUsize>();
    let node_offset = arena.alloc(size - not_used);
    // ... write key/value into arena memory
    node_offset  // Return OFFSET, not pointer
}
```

**Benefits**:
- No malloc per insert → O(1) allocation
- Nodes are contiguous → cache-friendly
- Offsets are `usize` → can be stored atomically for CAS

### 2. Lock-Free CAS (Compare-And-Swap)

**Problem with mutex**: Only one thread can modify at a time, poor scalability.

**Solution**: Use atomic `compare_exchange` to allow concurrent modifications.

```rust
// Atomically update: prev[i]->next[i] should go from old_next to new_node
let next_offset = arena.offset(next[i]);
x.tower[i].store(next_offset, Ordering::SeqCst);  // Store my next first

unsafe { &*prev[i] }.tower[i].compare_exchange(
    next_offset,           // Expected current value
    node_offset,          // Desired new value
    Ordering::SeqCst,      // Memory ordering
    Ordering::SeqCst,
) {
    Ok(_) => break,        // Success - no one else touched it
    Err(_) => {           // CAS failed - retry with new positions
        let (p, n) = unsafe { find_splice_for_level(&x.key, prev[i], i) };
        prev[i] = p;
        next[i] = n;
    }
}
```

**How CAS works**:
```
Thread A inserting NodeX between Node5 and Node7:
  Before: Node5.tower[0] = Node7 (offset)

  Thread A: compare_exchange(Node5.tower[0], Node7, NodeX_offset)
            │
            ├── If Node5.tower[0] is STILL Node7
            │      → Atomically set to NodeX_offset
            │      → Success!
            │
            └── If another thread changed it to something else
                   → Return Err with current value
                   → Retry with new positions
```

### 3. Atomic Height Management

```rust
struct SkiplistInner {
    height: AtomicUsize,  // Current max height of skiplist
    head: NonNull<Node>,
    arena: Arena,
}

// Lock-free height growth
if height > self.height() {
    self.inner.height.fetch_max(height, Ordering::SeqCst);
}

// Or CAS loop for concurrent updates
while height > list_height {
    match self.inner.height.compare_exchange_weak(
        list_height, height, Ordering::SeqCst, Ordering::SeqCst
    ) {
        Ok(_) => break,
        Err(h) => list_height = h,  // Height grew, retry
    }
}
```

### 4. Reverse Iteration (Prev Pointer)

**Problem**: SkipList only supports forward iteration. No way to go backwards efficiently.

**Solution**: Store offset to previous node at level 0.

```rust
pub struct Node {
    prev: AtomicUsize,  // Offset to previous node at level 0
    tower: [AtomicUsize; MAX_HEIGHT],
}

pub fn prev(&mut self) {
    if self.allow_concurrent_write {
        // Concurrent mode: use find_near to locate previous
        self.cursor = self.find_near(self.key(), true, false);
    } else {
        // Single-threaded: follow prev pointer directly
        let prev_offset = (*self.cursor).prev.load(Ordering::Acquire);
        self.cursor = self.arena.get_mut(prev_offset);
    }
}
```

### 5. Zero-Copy Keys (Bytes)

**Problem with generic K: Clone**: Every comparison might clone the key.

**Solution**: Use `bytes::Bytes` which is shared ownership (COW semantics).

```rust
pub struct Node {
    key: Bytes,      // Zero-copy reference
    value: Bytes,
    // ...
}

// Comparison doesn't clone
self.c.compare_key(key, &next.key)  // Both are Bytes references
```

**Bytes vs String vs Vec<u8>**:
| Type | Clone Cost | Comparison |
|------|-----------|------------|
| `String` | O(n) | May alloc |
| `Vec<u8>` | O(n) | May alloc |
| `Bytes` | O(1) | Zero-copy (just reference count) |

### 6. Geometric Height Distribution

**Formula**: `HEIGHT_INCREASE = u32::MAX / 3` (1/3 probability of increasing)

```rust
const HEIGHT_INCREASE: u32 = u32::MAX / 3;  // ~33% chance

fn random_height(&self) -> usize {
    let mut rng = rand::thread_rng();
    for h in 0..(MAX_HEIGHT - 1) {
        if !rng.gen_ratio(HEIGHT_INCREASE, u32::MAX) {
            return h;  // ~67% chance to return h
        }
    }
    MAX_HEIGHT - 1  // ~33% ^ 19 = very small chance
}
```

**Probability distribution**:
| Height | Probability |
|--------|-------------|
| 1 | 67% |
| 2 | 22% |
| 3 | 7% |
| 4+ | 4% |

**Average height ≈ 1.5** → O(log n) ≈ O(log n) with small constant.

## Comparison: StoneDB vs AgateDB SkipList

| Aspect | StoneDB (current) | AgateDB |
|--------|-------------------|---------|
| **Memory** | `Box<Node>` per insert | Arena allocator |
| **Concurrency** | Single-threaded | Lock-free CAS |
| **Pointers** | `*mut Node` raw | `AtomicUsize` offset |
| **Height** | `usize` | `AtomicUsize` |
| **Reverse** | Not supported | `prev` field |
| **Keys** | `K: Clone` | `Bytes` zero-copy |
| **Thread safety** | `unsafe impl` | Full `Send + Sync` |

## Next Steps

1. Rewrite SkipList using Arena allocator
2. Add lock-free CAS operations
3. Implement reverse iteration
4. Switch to zero-copy Bytes keys
5. Add concurrent write tests

## References

- [AgateDB skiplist](https://github.com/tikv/agatedb/blob/master/skiplist/src/list.rs)
- [AgateDB memtable](https://github.com/tikv/agatedb/blob/master/src/memtable.rs)
- [Rust Atomics and Locks](https://marabos.nl/atomics/) - CAS explanation
