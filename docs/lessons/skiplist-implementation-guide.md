# How to Implement a SkipList

**Date:** 2026-04-05
**Level:** Intermediate to Advanced
**Prerequisites:** Rust ownership, atomics, pointers, data structures basics

---

## Table of Contents

1. [What is a SkipList?](#what-is-a-skiplist)
2. [Why SkipList?](#why-skiplist)
3. [Key Design Decisions](#key-design-decisions)
4. [Data Structure Layout](#data-structure-layout)
5. [Core Operations](#core-operations)
6. [Implementation Details](#implementation-details)
7. [Concurrency with Lock-Free CAS](#concurrency-with-lock-free-cas)
8. [Our Implementation Walkthrough](#our-implementation-walkthrough)

---

## What is a SkipList?

A SkipList is a probabilistic data structure that provides **O(log n)** search, insert, and delete operations. It's essentially a linked list with "express lanes" (additional layers) that skip over groups of nodes.

```
Level 3: HEAD ------------------------------------------------> NIL
Level 2: HEAD -----------> node5 --------------------------> NIL
Level 1: HEAD --> node2 --> node5 --> node7 --> node9 -----> NIL
Level 0: HEAD --> node1 --> node2 --> node3 --> node5 --> ... --> NIL
```

**Key insight:** Each node appears in multiple layers. Higher levels = fewer nodes = faster "express" traversal.

---

## Why SkipList?

| Data Structure | Search | Insert | Delete | Ordered Iteration |
|----------------|--------|--------|--------|------------------|
| Array | O(log n) | O(n) | O(n) | O(1) |
| Linked List | O(n) | O(1) | O(1) | O(n) |
| **SkipList** | **O(log n)** | **O(log n)** | **O(log n)** | **O(n)** |
| Balanced Tree | O(log n) | O(log n) | O(log n) | O(n) |

SkipList offers similar complexity to balanced trees but with:
- **Simpler implementation** (no rotations)
- **Lock-free variants possible** (good for concurrency)
- **Better cache locality** than pointer-heavy trees

---

## Key Design Decisions

### 1. Arena Allocation (No Malloc Per Insert)

**Problem:** Traditional linked lists allocate each node with `malloc()`, which is slow.

**Solution:** Pre-allocate a large memory arena and bump-allocate nodes:

```rust
struct Arena {
    data: Vec<u8>,
    offset: usize,
}

impl Arena {
    pub fn alloc(&mut self, size: usize) -> usize {
        let offset = self.offset;
        self.offset += size;
        offset
    }
}
```

**Benefits:**
- O(1) allocation (just bump a pointer)
- No memory fragmentation
- Arena can be deallocated as a whole

### 2. Intrusive Data Structure

**Problem:** Traditional nodes store `Box<Node>` or `Rc<Node>`, adding heap overhead.

**Solution:** Embed forward pointers directly in the node, use offsets instead of pointers:

```rust
#[repr(C)]  // Ensure predictable memory layout
struct Node {
    key: Bytes,
    value: Bytes,
    height: usize,
    tower: [AtomicUsize; MAX_HEIGHT],  // Offsets, not pointers
}

struct SkiplistInner {
    head: usize,           // Offset to head node
    arena: Arc<Arena>,     // Shared arena
}
```

**Benefits:**
- Single memory allocation for entire node
- Offsets survive arena reallocation (if arena grows)
- Better memory density

### 3. Lock-Free CAS for Concurrency

**Problem:** Mutex locks are slow for concurrent writes.

**Solution:** Use Compare-And-Swap (CAS) for lock-free updates:

```rust
// Lock-free insert
loop {
    let next_offset = prev_node.tower[level].load(SeqCst);
    
    // ... find splice position ...
    
    match prev_node.tower[level].compare_exchange(
        next_offset,
        new_node_offset,
        SeqCst,
        SeqCst,
    ) {
        Ok(_) => break,      // Success!
        Err(_) => continue,  // Someone else modified, retry
    }
}
```

**Benefits:**
- Multiple threads can insert concurrently
- No mutex contention
- Scales with CPU cores

### 4. Pluggable KeyComparator

**Problem:** Hardcoded comparison breaks flexibility.

**Solution:** Trait-based comparator:

```rust
pub trait KeyComparator: Clone + Send + Sync {
    fn compare_key(&self, lhs: &[u8], rhs: &[u8]) -> Ordering;
    fn same_key(&self, lhs: &[u8], rhs: &[u8]) -> bool;
}

pub struct SkipList<CMP: KeyComparator> {
    cmp: CMP,
    // ...
}
```

**Use cases:**
- `BytewiseComparator`: Compare raw bytes
- `FixedLengthSuffixComparator`: Ignore sequence number suffix in LSM-tree keys

---

## Data Structure Layout

### Memory Layout (C-style, `#[repr(C)]`)

```
Node Layout (MAX_HEIGHT = 20):

┌──────────────────────────────────────────────────────────────┐
│ key: Bytes (16 bytes on stack as pointer + length)           │
├──────────────────────────────────────────────────────────────┤
│ value: Bytes (16 bytes)                                      │
├──────────────────────────────────────────────────────────────┤
│ height: usize (8 bytes)                                      │
├──────────────────────────────────────────────────────────────┤
│ prev: AtomicUsize (8 bytes) - for reverse iteration          │
├──────────────────────────────────────────────────────────────┤
│ tower[0]: AtomicUsize (8 bytes) - level 0 next              │
├──────────────────────────────────────────────────────────────┤
│ tower[1]: AtomicUsize (8 bytes)                              │
├──────────────────────────────────────────────────────────────┤
│ ...                                                          │
├──────────────────────────────────────────────────────────────┤
│ tower[19]: AtomicUsize (8 bytes) - level 19 next             │
└──────────────────────────────────────────────────────────────┘

Total: ~72 bytes base + 20 × 8 = 232 bytes per node
```

**Why `#[repr(C)`?**
- Predictable memory layout
- `tower` is at the END of the struct
- Can calculate exact offset to any field

### Why Tower at the Bottom?

For variable-size allocation:

```rust
// AgateDB-style: only allocate what you need
let not_used = (MAX_HEIGHT - height - 1) * mem::size_of::<AtomicUsize>();
let node_size = mem::size_of::<Node>() - not_used;
let offset = arena.alloc(node_size);
```

Tower at bottom → easier to calculate "unused" space at compile time.

---

## Core Operations

### Search

Goal: Find node with key, or find where to insert.

```
start at HEAD, level = max_height

while level >= 0:
    next = node.tower[level]
    
    if next == NIL:
        level--  // Drop down
    else if key < next.key:
        level--  // Go down
    else if key > next.key:
        node = next  // Move right
    else:
        return next  // Found!
```

Time complexity: O(log n) expected (probabilistic)

### Insert

1. **Find position** (same as search)
2. **Random height** (geometric distribution)
3. **Allocate node**
4. **Link in** (update tower pointers)

```
insert(key, value):
    // 1. Find predecessors at each level
    for level = max_height down to 0:
        prev[level] = find_splice(key, prev[level+1], level)
    
    // 2. Check for existing key
    if prev[0].tower[0] exists and prev[0].key == key:
        update value
        return
    
    // 3. Random height
    height = random_height()  // Geometric, 1/3 probability per level
    
    // 4. Allocate node
    node = alloc(height)
    
    // 5. Link in
    for level = 0 to height:
        node.tower[level] = prev[level].tower[level]
        prev[level].tower[level] = node
```

### Delete

Similar to insert, but unlink and optionally reclaim memory.

---

## Implementation Details

### Random Height (Geometric Distribution)

With probability P = 1/3, height = 1
With probability P = 1/9, height = 2
With probability P = 1/27, height = 3
...

```rust
const HEIGHT_INCREASE: u32 = u32::MAX / 3;

fn random_height(&self) -> usize {
    let mut rng = rand::thread_rng();
    for h in 0..(MAX_HEIGHT - 1) {
        if !rng.gen_ratio(HEIGHT_INCREASE, u32::MAX) {
            return h;
        }
    }
    MAX_HEIGHT - 1
}
```

**Why 1/3?** Classic choice. Makes expected tower length = 3/2.
- 67% of nodes have height 1 (just level 0)
- 22% have height 2
- 7% have height 3
- etc.

### Forward Iteration (Level-0 Linked List)

The level-0 tower forms a sorted linked list:

```rust
// O(n) iteration - just follow level-0 pointers
fn iter_ref(&self) -> IterRef<'_, CMP> {
    IterRef { list: self, cursor: 0 }
}

impl IterRef {
    pub fn next(&mut self) {
        let node = &*get_node(self.cursor);
        self.cursor = node.tower[0].load(SeqCst);
    }
}
```

**Key insight:** Level-0 is just a regular linked list!

### Reverse Iteration (Prev Pointer)

For reverse iteration, maintain a `prev` pointer at level 0:

```rust
struct Node {
    // ...
    prev: AtomicUsize,  // Offset to previous node at level 0
    tower: [AtomicUsize; MAX_HEIGHT],
}

// On insert at level 0:
node.prev.store(prev[0], Relaxed);
if next != NIL {
    next_node.prev.store(node_offset, Release);
}
```

---

## Concurrency with Lock-Free CAS

### The Insert Race Condition

```
Thread A: wants to insert node X between prev and next
Thread B: wants to insert node Y between prev and next

Problem: Both threads see prev.tower[level] = next
         Both try to CAS from next to their node
         One will fail!
```

### CAS Solution

```rust
loop {
    let next_offset = prev_node.tower[level].load(SeqCst);
    
    // ... verify prev is still correct ...
    
    // Set our next first (store-only, no CAS needed)
    new_node.tower[level].store(next_offset, SeqCst);
    
    // CAS: only succeeds if no one else modified prev.tower[level]
    match prev_node.tower[level].compare_exchange(
        next_offset,
        node_offset,
        SeqCst,
        SeqCst,
    ) {
        Ok(_) => break,     // We won the race!
        Err(actual) => {
            // Someone else modified it, re-search and retry
            prev = find_splice(key, prev, level);
            next_offset = actual;
            continue;
        }
    }
}
```

### Why It Works

1. **Atomic swap**: Only one thread can successfully CAS any given pointer
2. **Retry on failure**: If CAS fails, someone else modified our predecessor - re-search and retry
3. **No locks**: Threads never wait for each other

### Memory Ordering

```rust
// Relaxed: Just atomics, no synchronization
node.tower[level].store(offset, Relaxed);

// Release: All prior writes visible before this store
prev_node.tower[level].store(node_offset, Release);

// Acquire: Subsequent reads see all Release stores
let next = node.tower[level].load(Acquire);

// SeqCst: Full sequential consistency (strongest, slowest)
```

---

## Our Implementation Walkthrough

### File Structure

```
crates/stonedb-core/src/skiplist/
├── mod.rs       # Main SkipList + IterRef implementation
├── arena.rs    # Arena allocator
└── key.rs      # KeyComparator trait + implementations
```

### Node Structure

```rust
#[repr(C)]
struct Node {
    key: Bytes,
    value: Bytes,
    height: usize,
    prev: AtomicUsize,
    tower: [AtomicUsize; MAX_HEIGHT],
}
```

### SkipList Structure

```rust
struct SkiplistInner {
    height: AtomicUsize,      // Current max height
    head: usize,              // Head node offset
    arena: Arc<Arena>,         // Shared arena
}

#[derive(Clone)]
pub struct SkipList<CMP: KeyComparator> {
    inner: Arc<SkiplistInner>,
    cmp: CMP,
    allow_concurrent_write: bool,
}
```

### Insert Algorithm (Simplified)

```rust
pub fn put(&self, key: &[u8], value: &[u8]) -> Option<(Bytes, Bytes)> {
    // 1. Random height
    let height = self.random_height();
    
    // 2. Find predecessors
    let mut prev = [0usize; MAX_HEIGHT + 1];
    let mut next = [0usize; MAX_HEIGHT + 1];
    
    for level in (0..=self.list_height()).rev() {
        let (p, n) = self.find_splice_for_level(key, prev[level + 1], level);
        prev[level] = p;
        next[level] = n;
        
        // Check duplicate
        if p == n && p != 0 {
            // Key exists - update or ignore
        }
    }
    
    // 3. Allocate node
    let node_offset = Node::alloc(&self.inner.arena, key, value, height);
    
    // 4. Link in (lock-free for concurrent mode)
    for level in 0..=height {
        if self.allow_concurrent_write {
            // CAS loop
        } else {
            // Direct store
            new_node.tower[level].store(next[level], Relaxed);
            prev_node.tower[level].store(node_offset, Release);
        }
    }
}
```

---

## Further Study

### Topics Not Covered

1. **Memory reclamation**: Lock-free deletion is complex (hazard pointers, epoch-based)
2. **Variable-size nodes**: AgateDB calculates exact node size
3. **Arena growth**: When arena is full, allocate new arena and copy
4. **Batch operations**: Multi-element insert/delete atomically

### References

- [SkipList Original Paper](https://www.epaper.dev/sites/default/files/07P92.pdf) - Pugh 1990
- [AgateDB SkipList](https://github.com/tikv/agatedb/tree/master/skiplist) - Production implementation
- [Java Concurrency in Practice](https://www.amazon.com/Java-Concurrency-Practice-Brian-Goetz/dp/0321349601) - Great for lock-free patterns

---

## Exercises

1. **Implement `contains()`** using `find_splice_for_level`
2. **Implement `len()`** by traversing level-0
3. **Add `seek_for_prev()`** to IterRef (find largest key <= target)
4. **Implement variable-size node allocation** (allocate only needed tower levels)
5. **Add reverse iterator** using `prev` pointer

---

*Last updated: 2026-04-05*
