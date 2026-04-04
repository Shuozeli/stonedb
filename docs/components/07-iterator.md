# Iterator

## Purpose

Iterators provide **range scan** capabilities across the LSM tree, and support point lookups (Get).

## RocksDB Reference

| File | Purpose |
|------|---------|
| `rocksdb/include/rocksdb/iterator.h` | Iterator interface |
| `rocksdb/db/merge_iterators.h` | MergingIterator |
| `rocksdb/table/two_level_iterator.cc` | Two-level SSTable iterator |
| `rocksdb/db/table_iterators.h` | Table-level iterators |
| `rocksdb/db/memtable_list.h` | MemTable iterator for immutable list |

**Key line references:**
- Iterator interface: `iterator.h:24-62`
- MergingIterator: `merge_iterators.h:1-100`
- TwoLevelIterator: `two_level_iterator.cc:1-150`
- DBIter (DB-level iterator): `db_iter.cc` - combines MemTable + SSTables

## Core Principles

1. **Sorted order**: Iterate keys in sorted InternalKey order
2. **Merged view**: Combine MemTables + all SSTables seamlessly
3. **Lazy I/O**: Only read blocks when needed
4. **No isolation anomalies**: Snapshot isolation by default

## RocksDB Iterator Hierarchy

```
┌────────────────────────────────────────────────────────────────────┐
│                         Iterator Hierarchy                           │
├────────────────────────────────────────────────────────────────────┤
│                                                                      │
│                     Iterator (base interface)                        │
│                      rocksdb/include/rocksdb/iterator.h             │
│                              │                                       │
│         ┌───────────────────┼───────────────────┐                   │
│         │                   │                   │                    │
│         ▼                   ▼                   ▼                    │
│   DBIter           MergingIterator      ExternalIterator           │
│   (full DB)       (multi-source)                                 │
│                              │                                       │
│         ┌───────────────────┼───────────────────┐                   │
│         │                   │                   │                    │
│         ▼                   ▼                   ▼                    │
│   MemTable           SSTableIterator   ColumnFamilyIterator        │
│   Iterator                                         │                │
│                              │                    │                │
│                              ▼                    ▼                │
│                    TwoLevelIterator       TwoLevelIterator         │
│                    (index + data)                                  │
│                                                                      │
└────────────────────────────────────────────────────────────────────┘
```

## RocksDB Iterator Interface

```cpp
// rocksdb/include/rocksdb/iterator.h:24-62
class Iterator {
public:
  virtual ~Iterator();

  // Position
  virtual void Seek(const Slice& target) = 0;
  virtual void SeekToFirst() = 0;
  virtual void SeekToLast() = 0;

  // Movement
  virtual void Next() = 0;
  virtual void Prev() = 0;

  // Access
  virtual bool Valid() const = 0;
  virtual Slice key() const = 0;
  virtual Slice value() const = 0;

  // Status
  virtual Status status() const = 0;
};
```

## StoneDB Iterator Trait

```rust
// Simple, idiomatic Rust iterator
pub trait Iterator {
    fn seek(&mut self, key: &[u8]);
    fn seek_to_first(&mut self);
    fn seek_to_last(&mut self);

    fn next(&mut self);
    fn prev(&mut self);

    fn key(&self) -> &[u8];
    fn value(&self) -> &[u8];
    fn valid(&self) -> bool;

    fn status(&mut self) -> Result<()>;
}
```

## MemTable Iterator

### RocksDB MemTable Iterator

```cpp
// rocksdb/db/memtable_list.h
// MemTableListIterator wraps SkipListIterator for immutable memtables
class MemTableListIterator : public Iterator {
  // Simple wrapper over SkipList iterator
  // No two-level access needed - skiplist is memory-only
};
```

### StoneDB MemTable Iterator

```rust
pub struct MemTableIterator<'a> {
    iter: SkipListIter<'a>,
}

impl MemTableIterator {
    pub fn seek(&mut self, key: &[u8]) {
        // Encode key with MAX sequence for this user_key
        let search_key = encode_internal_key_max(key);
        self.iter.seek_to(&search_key);
    }

    pub fn seek_to_first(&mut self) {
        self.iter.seek_to_first();
    }

    pub fn next(&mut self) {
        self.iter.next();
    }

    pub fn key(&self) -> &[u8] {
        decode_internal_key(self.iter.key()).user_key
    }
}
```

## SSTable Iterator (Two-Level)

### RocksDB Two-Level Iterator

```cpp
// rocksdb/table/two_level_iterator.cc:1-150
// Level 1: Index block iterator (positions within index)
// Level 2: Data block iterator (positions within data block)

// Created by BlockBasedTable::NewIterator()
class TwoLevelIterator : public Iterator {
  IteratorWrapper index_iter_;      // For index block
  IteratorWrapper data_iter_;      // For data block
  bool (*index_block_function_)(); // Callback to read index

  void Seek(const Slice& target) override;
  void Next() override;
};
```

### StoneDB SSTable Iterator

```rust
pub struct SSTableIterator {
    table: Arc<Table>,
    index_iter: BlockIterator,
    data_iter: Option<BlockIterator>,
    options: IteratorOptions,
}

impl SSTableIterator {
    fn seek_to_block(&mut self, key: &[u8]) -> Result<()> {
        // 1. Check bloom filter first (fast reject)
        if !self.table.bloom_filter.may_contain(key) {
            return Ok(());
        }

        // 2. Binary search index block to find data block
        let block_info = self.index_iter.seek_to_block(key)?;

        // 3. Read data block
        self.data_iter = Some(self.table.read_block(&block_info)?);

        // 4. Seek to exact key within block
        self.data_iter.as_mut().unwrap().seek(key);

        Ok(())
    }
}
```

## Merging Iterator

### RocksDB MergingIterator

```cpp
// rocksdb/db/merge_iterators.h:1-100
// Merges multiple sorted iterators, skipping older entries with same key
class MergingIterator : public Iterator {
  std::vector<IteratorWrapper> children_;
  InternalKeyComparator comparator_;

  // Current state
  IteratorWrapper* current_;
  bool direction_;

  void Next() override;
  void Seek(const Slice& target) override;
};

// Key merge logic:
// 1. Find child with smallest key
// 2. If multiple children have same key, skip lower sequence numbers
// 3. Return only the newest entry
```

### StoneDB MergingIterator

```rust
pub struct MergingIterator {
    children: Vec<Box<dyn Iterator>>,
    cmp: InternalKeyComparator,
    current_index: Option<usize>,
}

impl MergingIterator {
    pub fn seek(&mut self, key: &[u8]) {
        // 1. Seek all children to key
        for child in &mut self.children {
            child.seek(key);
        }

        // 2. Find smallest key among valid children
        self.find_current();
    }

    pub fn next(&mut self) {
        if let Some(idx) = self.current_index {
            // Advance the child that provided current key
            self.children[idx].next();

            // Find next smallest key
            self.find_current();
        }
    }

    fn find_current(&mut self) {
        let mut best_key: Option<&[u8]> = None;
        let mut best_idx: Option<usize> = None;

        for (i, child) in self.children.iter_mut().enumerate() {
            if !child.valid() {
                continue;
            }

            let key = child.key();
            match best_key {
                None => {
                    best_key = Some(key);
                    best_idx = Some(i);
                }
                Some(best) => {
                    if self.cmp.compare(key, best) < 0 {
                        best_key = Some(key);
                        best_idx = Some(i);
                    }
                }
            }
        }

        self.current_index = best_idx;
    }
}
```

### Merging Algorithm (Same Key Handling)

```
Input sources (all sorted by InternalKey):
  MemTable:    [a:100:Put, b:50:Put, c:25:Put]
  L0[0]:       [a:90:Put,  b:40:Delete, d:30:Put]
  L1[0]:       [a:10:Put,  b:10:Put,    e:10:Put]

Merge process:
  1. Find smallest key: 'a' (present in all sources)
     - Keep highest sequence: a:100:Put (from MemTable)
     - Advance MemTable iterator

  2. Next smallest: 'b' (present in all sources)
     - MemTable has b:50:Put (highest, keep)
     - L0 has b:40:Delete (lower, skip)
     - L1 has b:10:Put (lower, skip)
     - Advance MemTable iterator

  3. Next smallest: 'c' (MemTable only)
     - c:25:Put (keep)
     - Advance MemTable iterator

  4. Next smallest: 'd' (L0 only)
     - d:30:Put (keep)
     - Advance L0 iterator

  5. Next smallest: 'e' (L1 only)
     - e:10:Put (keep)
     - Advance L1 iterator

Final output: a:100:Put, b:50:Put, c:25:Put, d:30:Put, e:10:Put
```

## DB Iterator (Full LSM Tree)

```rust
/// Iterates over entire LSM tree: MemTables + all levels
pub struct DBIter {
    merging_iter: MergingIterator,
    snapshot: Snapshot,
}

impl DBIter {
    pub fn seek(&mut self, key: &[u8]) {
        // Seek merging iterator (handles all sources)
        self.merging_iter.seek(key);

        // Skip deleted entries
        self.skip_tombstones();
    }

    fn skip_tombstones(&mut self) {
        while self.merging_iter.valid() {
            if is_tombstone(self.merging_iter.value()) {
                self.merging_iter.next();
            } else {
                break;
            }
        }
    }
}
```

## Snapshot Isolation

```rust
pub struct Snapshot {
    pub sequence: u64,
    db: Arc<Database>,
}

/// Iterator bound to a specific snapshot
pub struct SnapshotIterator<I: Iterator> {
    iter: I,
    snapshot: Snapshot,
}

impl<I: Iterator> Iterator for SnapshotIterator<I> {
    fn seek(&mut self, key: &[u8]) {
        // Encode key with snapshot's sequence number
        let search_key = encode_internal_key_with_seq(key, self.snapshot.sequence);
        self.iter.seek(&search_key);
    }

    // Only sees entries with sequence <= snapshot.sequence
}
```

## Block Iterator (Within SSTable Block)

```rust
/// Iterator within a single data block
pub struct BlockIterator {
    block: Arc<Block>,
    restarts: Vec<u32>,        // Restart point offsets
    num_restarts: u32,
    current_index: i32,         // -1 if not started
}

impl BlockIterator {
    pub fn seek(&mut self, key: &[u8]) {
        // 1. Binary search restart points to find candidate region
        let mut lo = 0;
        let mut hi = self.num_restarts as i32 - 1;

        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let restart_key = self.block.key_at_restart(mid as u32);
            if self.cmp.compare(&restart_key, key) <= 0 {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }

        // 2. Seek to restart point, then linear scan
        self.current_index = lo;
        self.seek_to_key_within_block(key);
    }

    fn seek_to_key_within_block(&mut self, key: &[u8]) {
        // Linear scan from restart point until key >= target
        // Apply delta decoding at each step
    }
}
```

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `iterator/trait.rs` | Iterator trait | `iterator.h` |
| `iterator/merge.rs` | MergingIterator | `merge_iterators.h` |
| `iterator/two_level.rs` | Two-level SSTable iterator | `two_level_iterator.cc` |
| `iterator/block.rs` | Block iterator | `block.cc` |
| `iterator/memtable.rs` | MemTable iterator | `memtable_list.h` |
| `iterator/db.rs` | DBIter | `db_iter.cc` |
| `iterator/snapshot.rs` | SnapshotIterator | `db_iter.cc` |

## Performance Considerations

| Optimization | Description |
|--------------|-------------|
| Seek optimization | Only read index block on seek |
| Block cache | Cache data blocks after read |
| Prefix bloom | Skip SSTables that can't have key |
| Early termination | Stop at seek target |
| Direction-aware merge | More efficient for prev() calls |

## Implementation Notes

- **Use concrete types** (`Bytes`) not GAT lifetimes for simplicity
- **MergingIterator** is the heart of read performance
- **Two-level indexing** minimizes I/O on seeks
- **Snapshot isolation** via sequence number filtering
- **Lazy I/O**: Don't read blocks until iterator positioned

## Status

**Not started** - Depends on MemTable and SSTable iterators.
