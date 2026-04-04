# MemTable

## Purpose

MemTable is the **in-memory sorted structure** where all writes go first. It provides fast writes and serves as the first layer in the read path.

## RocksDB Reference

| File | Purpose |
|------|---------|
| `rocksdb/db/memtable.h` | MemTable interface |
| `rocksdb/memtable/skiplist.h` | SkipList memtable implementation |
| `rocksdb/db/memtable_list.h` | Immutable memtable list management |
| `rocksdb/db/dbformat.h` | InternalKey, SequenceNumber, ValueType |
| `rocksdb/include/rocksdb/db.h` | DB interface for write operations |

**Key line references:**
- InternalKey format: `dbformat.h:83-102` - user_key | sequence (56-bit) | value_type (8-bit)
- ValueType enum: `dbformat.h:70-79` - kTypeValue, kTypeDeletion, kTypeSingleDeletion
- SkipList insert: `skiplist.h:180-195`
- Sequence number allocation: `db_impl_write.cc:800-850`

## Core Principles

1. **Sorted by key**: All entries maintained in sorted InternalKey order
2. **In-memory only**: No durability (WAL provides that)
3. **O(log n) operations**: Fast insert and lookup
4. **Immutable transition**: When full, becomes immutable and triggers flush

## RocksDB Design

### InternalKey Format

```cpp
// rocksdb/db/dbformat.h:83-102
// InternalKey = user_key | sequence_number (56 bits) | value_type (8 bits)
struct ParsedInternalKey {
  Slice user_key;       // The actual key
  uint64_t sequence;    // 56-bit sequence number (higher = newer)
  ValueType type;       // kTypeValue, kTypeDeletion, etc.
};
```

### Sequence Number

```cpp
// rocksdb/db/dbformat.h:104-118
// Sequence number is monotonically increasing
// Each write gets a unique sequence number
// Higher sequence = newer entry
// Max sequence = 0x00FFFFFFFFFFFFFF (7 bytes)
```

### ValueType Enum

```cpp
// rocksdb/db/dbformat.h:70-79
enum ValueType {
  kTypeDeletion = 0x0,
  kTypeValue = 0x1,
  // ... other types
};
```

### SkipList MemTable

```cpp
// rocksdb/memtable/skiplist.h:180-195
template <typename Key, class Comparator>
class SkipList {
  // Insert must not be called concurrently with other inserts
  void Insert(const Key& key);

  // Contains must not be called concurrently with modifications
  bool Contains(const Key& key) const;
};
```

### MemTable List (Immutable Management)

```cpp
// rocksdb/db/memtable_list.h
class MemTableList {
  // Manages multiple immutable MemTables being flushed
  // Non-blocking reads while flush in progress
};
```

## StoneDB MemTable Design

### InternalKey Encoding

```
┌──────────────────────────────────────────────────────────────┐
│  InternalKey                                                   │
├──────────────────────────────────────────────────────────────┤
│  user_key (variable) | sequence_number (56 bits) | type (8 bits) │
└──────────────────────────────────────────────────────────────┘

// For Little-Endian encoding:
// sequence (7 bytes) | type (1 byte) appended after user_key
// Decoding: read last 8 bytes, extract sequence and type
```

### Entry Structure

```rust
pub struct Entry {
    pub key: Vec<u8>,         // InternalKey encoded as bytes
    pub value: Bytes,         // Value or tombstone
}

pub struct InternalKey {
    pub user_key: Vec<u8>,
    pub sequence: u64,
    pub value_type: ValueType,
}

#[repr(u8)]
pub enum ValueType {
    Delete = 0x0,
    Value = 0x1,
}
```

### SkipList Node Structure (Rust)

```rust
// Based on rocksdb/memtable/skiplist.h
const MAX_HEIGHT: usize = 20;  // Enough for 1M+ elements

pub struct Node {
    pub key: Bytes,                    // Key bytes
    pub value: Bytes,                  // Value bytes
    height: usize,                     // Random height
    tower: Vec<AtomicPtr<Node>>,       // Skip list levels [AtomicPtr; MAX_HEIGHT]
}
```

## Key Operations

### Insert (Rust)

```rust
impl MemTable {
    pub fn put(&self, key: &[u8], value: &[u8]) -> Result<u64> {
        // 1. Allocate sequence number (atomic increment)
        let seq = self.next_sequence.fetch_add(1, Ordering::AcqRel);

        // 2. Encode InternalKey (user_key | seq | kTypeValue)
        let internal_key = encode_internal_key(key, seq, ValueType::Value);

        // 3. Insert into skiplist
        self.skiplist.insert(internal_key, value);

        Ok(seq)
    }

    pub fn delete(&self, key: &[u8]) -> Result<u64> {
        let seq = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        let internal_key = encode_internal_key(key, seq, ValueType::Delete);
        self.skiplist.insert(internal_key, &[]);  // Tombstone
        Ok(seq)
    }
}
```

### Get (Point Lookup)

```rust
impl MemTable {
    pub fn get(&self, key: &[u8]) -> Result<Option<Bytes>> {
        // Encode InternalKey with MAX sequence for this key
        let search_key = encode_internal_key_max(key);

        // Search skiplist for most recent entry
        match self.skiplist.get(&search_key) {
            Some(value) if !is_tombstone(value) => Ok(Some(value)),
            Some(_) => Ok(None),  // Tombstone
            None => Ok(None),
        }
    }
}
```

### Iterator

```rust
impl Iterator for MemTableIterator {
    fn seek(&mut self, key: &[u8]) {
        let search_key = encode_internal_key_max(key);
        self.iter.seek_to_key(&search_key);
    }

    fn next(&mut self) {
        self.iter.next();
    }

    fn key(&self) -> &[u8] {
        decode_internal_key(self.iter.key()).user_key
    }

    fn value(&self) -> &[u8] {
        self.iter.value()
    }
}
```

## Flush to SSTable

```
MemTable (full)
      │
      ▼
┌─────────────────┐
│ Make Immutable  │  ◄── Stop accepting writes
└────────┬────────┘
         │ (async background)
         ▼
┌─────────────────┐
│  Flush to SST   │  ◄── Build SSTable from skiplist iteration
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Delete WAL     │  ◄── WAL entries for this memtable no longer needed
└─────────────────┘
```

RocksDB flush flow (`db_impl_compaction_flush.cc`):
```cpp
// BackgroundFlush() writes memtable to L0 SSTable
// LogAndApply() updates MANIFEST with new SSTable
// DeleteMinimalMemtable() removes flushed memtable
```

## Connection to WAL

```
Write Request:
      │
      ▼
┌─────────────┐
│    WAL      │ ◄── 1. Write batch to WAL
└──────┬──────┘      2. fsync() WAL
       │
       ▼
┌─────────────┐
│  MemTable   │ ◄── 3. Insert into skiplist (after WAL synced)
└─────────────┘

Recovery:
      │
      ▼
┌─────────────┐
│  WAL Replay │ ◄── Read WAL, reconstruct MemTable
└──────┬──────┘
       │
       ▼
┌─────────────┐
│  MemTable   │ ◄── Rebuild skiplist from WAL entries
└─────────────┘
```

## Arena Allocator

RocksDB uses arena allocation for skiplist nodes (`memtable/arena.h`):

```cpp
// rocksdb/memtable/arena.h
class Arena {
  char* alloc_(size_t bytes);
  // Pre-allocated buffer with bump allocator
  // No per-node malloc overhead
};
```

In Rust, consider `bumpalo` crate or custom arena.

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `memtable/mod.rs` | MemTable struct | `rocksdb/db/memtable.h` |
| `memtable/skiplist.rs` | SkipList implementation | `rocksdb/memtable/skiplist.h` |
| `memtable/arena.rs` | Memory arena | `rocksdb/memtable/arena.h` |
| `memtable/key.rs` | InternalKey encoding | `rocksdb/db/dbformat.h` |
| `memtable/iterator.rs` | MemTable iterator | `skiplist.h` Iterator |

## Implementation Notes

- **Sequence numbers are atomic** - Use `AtomicU64` with `fetch_add`
- **InternalKey encoding** - Append 8 bytes after user_key: seq (7 bytes) + type (1 byte)
- **SkipList vs BTreeMap** - SkipList chosen per RocksDB/LevelDB convention
- **Immutable MemTable** - When full, mark immutable and start new one
- **No locking for reads** - SkipList allows lock-free reads with CAS

## Status

**Not started** - Depends on WAL (sequence numbers assigned after WAL write).
