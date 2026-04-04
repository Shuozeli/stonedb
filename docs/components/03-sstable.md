# SSTable (Sorted String Table)

## Purpose

SSTable is the **on-disk persistent storage** for sorted key-value data. Once a MemTable is flushed, its contents become one or more SSTables in L0.

## RocksDB Reference

| File | Purpose |
|------|---------|
| `rocksdb/table/format.h` | BlockHandle, Footer, file format |
| `rocksdb/table/block_based/block.h` | Block format |
| `rocksdb/table/block_based/block_builder.cc` | Block building with delta encoding |
| `rocksdb/table/block_based/block.cc` | Block reading |
| `rocksdb/table/block_based/filter_policy.cc` | Bloom filter implementation |
| `rocksdb/table/block_based/filter_block.h` | Filter block format |
| `rocksdb/table/block_based/block_based_table_builder.cc` | SSTable builder |
| `rocksdb/table/block_based/block_based_table_reader.cc` | SSTable reader |
| `rocksdb/table/block_based/table_builder.cc` | Table builder interface |
| `rocksdb/include/rocksdb/table.h` | Table options, block cache config |
| `rocksdb/table/block_based/index.h` | Index block implementation |
| `rocksdb/table/block_based/index_builder.cc` | Index block builder |

**Key line references:**
- Block size default: `table.h:338` - `uint64_t block_size = 4 * 1024;`
- Block format: `block.h:18-45` - data + restart points + checksum
- Restart interval: `block_builder.cc:16` - `static constexpr int kRestartInterval = 16;`
- Bloom filter: `filter_policy.cc:365-632` - FastLocalBloom with XXH3-64
- Footer format: `format.h:78-95` - metaindex_handle + index_handle + magic

## Core Principles

1. **Immutable on disk**: Once written, never modified
2. **Sorted by key**: Entries stored in sorted InternalKey order
3. **Block-based**: Organized into fixed-size blocks (default 4KB)
4. **Self-describing**: Includes indexes, filters, and metadata

## RocksDB SSTable File Format

```
┌──────────────────────────────────────────────────────────────────┐
│                        SSTable File                               │
├──────────────────────────────────────────────────────────────────┤
│                                                                   │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌───────────┐│
│  │  Data Block │ │  Data Block │ │  Data Block │ │     ...   ││
│  │   1         │ │   2         │ │   3         │ │           ││
│  │ (4KB default)│ │ (4KB default)│ │ (4KB default)│ │           ││
│  └──────┬──────┘ └──────┬──────┘ └──────┬──────┘ └───────────┘│
│         │                │                │                      │
│         └────────────────┴────────────────┘                      │
│                           │                                       │
│                           ▼                                        │
│                  ┌─────────────────┐                               │
│                  │  Index Block    │ ◄─── Key range → block offset│
│                  └────────┬────────┘                               │
│                           │                                       │
│                           ▼                                        │
│                  ┌─────────────────┐                               │
│                  │  MetaIndex Block│ ◄─── Points to filter, props │
│                  └────────┬────────┘                               │
│                           │                                       │
│                           ▼                                        │
│                  ┌─────────────────┐                               │
│                  │  Properties Block│ ◄─── Metadata                │
│                  └────────┬────────┘                               │
│                           │                                       │
│  ┌────────────────────────────────────────────────────────────────┤
│  │                         Footer (48 bytes)                      │
│  ├────────────────────────────────────────────────────────────────┤
│  │  metaindex_handle (H) │ index_handle (H) │ padding │ magic    │
│  └────────────────────────────────────────────────────────────────┘
└──────────────────────────────────────────────────────────────────┘

Legend: H = BlockHandle (offset + size varint encoded)
```

## RocksDB Block Format

### Data Block

```cpp
// rocksdb/table/block_based/block_builder.cc:10-36
// Format:
// [entry 1: shared_bytes|unshared_bytes|value_len|key_delta|value]
// [entry 2: shared_bytes|unshared_bytes|value_len|key_delta|value]
// ...
// [restart point 1: u32 offset]
// [restart point 2: u32 offset]
// ...
// [num_restarts: u32]
// [block trailer: compression_type (1B) + crc32c (4B)]
```

### Restart Points

```cpp
// rocksdb/table/block_based/block_builder.cc:16
static constexpr int kRestartInterval = 16;  // Full key every 16 entries
```

Every `kRestartInterval` (default 16) entries, a **restart point** stores the full key (shared_bytes = 0). Between restart points, keys are delta-encoded.

### BlockHandle

```cpp
// rocksdb/table/format.h:28-45
class BlockHandle {
  uint64_t offset_;
  uint64_t size_;
  // Encoded as two varint64 values
};
```

## Bloom Filter (RocksDB)

```cpp
// rocksdb/table/block_based/filter_policy.cc:365-632
// FastLocalBloom (format_version >= 5)
// Uses XXH3-64 hash function

// rocksdb/util/hash.h:81-88
inline uint64_t GetSliceHash64(const Slice& s) {
  return XXH3_64bits(s.data(), s.size());
}
```

**Configuration:**
```cpp
// rocksdb/include/rocksdb/table.h
struct BlockBasedTableOptions {
  // Default: 10 bits per key
  double bits_per_key = 10;

  // Use Ribbon filter (more space efficient)
  bool use_ribbon_filter = false;
};
```

## StoneDB SSTable Design

### File Structure

```
┌──────────────────────────────────────────────────────────────────┐
│                        SSTable File                               │
├──────────────────────────────────────────────────────────────────┤
│                                                                   │
│  ┌─────────────┐ ┌─────────────┐ ┌─────────────┐ ┌───────────┐│
│  │  Data Block │ │  Data Block │ │  Data Block │ │     ...   ││
│  │   1         │ │   2         │ │   3         │ │           ││
│  └──────┬──────┘ └──────┬──────┘ └──────┬──────┘ └───────────┘│
│         └────────────────┴────────────────┘                      │
│                           │                                       │
│                           ▼                                        │
│                  ┌─────────────────┐                               │
│                  │  Index Block    │ ◄─── 1 entry per data block │
│                  └────────┬────────┘                               │
│                           │                                       │
│                           ▼                                        │
│                  ┌─────────────────┐                               │
│                  │  Bloom Filter   │ ◄─── 1 per SSTable          │
│                  │  (in MetaIndex) │                               │
│                  └────────┬────────┘                               │
│                           │                                       │
│  ┌────────────────────────────────────────────────────────────────┤
│  │                         Footer (48 bytes)                      │
│  ├────────────────────────────────────────────────────────────────┤
│  │  offset (varint64) │ size (varint64) │ ... │ magic (8B)       │
│  └────────────────────────────────────────────────────────────────┘
└──────────────────────────────────────────────────────────────────┘
```

### Block Structure (Data Block)

```
┌────────────────────────────────────────────────────────────────────┐
│  Data Block                                                         │
├────────────────────────────────────────────────────────────────────┤
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ Entry 1: shared(0) | unshared | value_len | key | value    │  │
│  │ Entry 2: shared(4) | unshared | value_len | key_delta | val │  │
│  │ ...                                                        │  │
│  │ Entry 16: shared(0) | ... (restart point)                  │  │
│  │ Entry 17: shared(3) | ...                                  │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ Restart Points: [u32; num_restarts]                         │  │
│  │ num_restarts: u32                                           │  │
│  └──────────────────────────────────────────────────────────────┘  │
│  ┌──────────────────┐                                           │
│  │ compression (1B) │                                           │
│  │ crc32c (4B)      │                                           │
│  └──────────────────┘                                           │
└────────────────────────────────────────────────────────────────────┘
```

## SSTable Builder (Rust)

```rust
// rocksdb/table/block_based/block_based_table_builder.cc is reference
// Key method: Add()

pub struct TableBuilder {
    block_builder: BlockBuilder,
    index_builder: IndexBuilder,
    bloom_filter: BloomFilterBuilder,
    options: TableOptions,
}

impl TableBuilder {
    pub fn add(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        // 1. Check if current data block is full
        if self.block_builder.approximate_size() >= self.options.block_size {
            self.finish_data_block()?;
        }

        // 2. Add to block builder (handles delta encoding)
        self.block_builder.add(key, value);

        // 3. Update bloom filter
        self.bloom_filter.add(key);

        Ok(())
    }

    pub fn finish(&mut self) -> Result<TableProperties> {
        // 1. Finish last data block
        self.finish_data_block()?;

        // 2. Write index block
        // 3. Write bloom filter
        // 4. Write meta index
        // 5. Write footer
        // 6. Return file size, key count, etc.
    }
}
```

## SSTable Reader (Rust)

```rust
// rocksdb/table/block_based/block_based_table_reader.cc is reference
// Key methods: Get(), NewIterator()

pub struct Table {
    file: Mmap,
    footer: Footer,
    index_block: Block,
    bloom_filter: BloomFilter,
    options: TableOptions,
}

impl Table {
    pub fn get(&self, key: &[u8]) -> Result<Option<Bytes>> {
        // 1. Check bloom filter (fast reject)
        if !self.bloom_filter.may_contain(key) {
            return Ok(None);
        }

        // 2. Binary search index block to find block handle
        let block_handle = self.index_block.search(key)?;

        // 3. Read data block
        let block = self.read_block(&block_handle)?;

        // 4. Binary search within block (using restart points)
        block.search(key)
    }
}
```

## Block Search (Binary Search with Restart Points)

```rust
impl Block {
    // 1. Binary search restart points to find candidate region
    // 2. Linear scan within region to exact key
    pub fn search(&self, key: &[u8]) -> Option<Bytes> {
        // Binary search restart array
        let mut lo = 0;
        let mut hi = self.num_restarts;

        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let restart_key = self.restart_key(mid);
            if compare_keys(&restart_key, key) <= 0 {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }

        // Linear scan from restart point
        self.scan_from_restart(lo, key)
    }
}
```

## Compression

RocksDB supports multiple codecs (`include/rocksdb/compression_type.h`):

| Codec | Speed | Ratio |
|-------|-------|-------|
| None | - | 1x |
| LZ4 | ~2 GB/s | 2-3x |
| ZSTD | ~1 GB/s | 3-5x |
| Snappy | ~0.5 GB/s | 2-3x |
| Zlib | ~0.2 GB/s | 3-5x |

**Recommendation**: LZ4 for hot data, ZSTD for cold data.

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `sstable/mod.rs` | Table struct | `block_based_table_reader.cc` |
| `sstable/builder.rs` | Table builder | `block_based_table_builder.cc` |
| `sstable/reader.rs` | Table reader | `block_based_table_reader.cc` |
| `sstable/block.rs` | Block format | `block.h`, `block.cc` |
| `sstable/block_builder.rs` | Block building | `block_builder.cc` |
| `sstable/index.rs` | Index block | `index.h`, `index_builder.cc` |
| `sstable/filter.rs` | Bloom filter | `filter_policy.cc` |
| `sstable/footer.rs` | Footer format | `format.h` |
| `sstable/handle.rs` | BlockHandle | `format.h` |

## Implementation Notes

- **Block size**: Default 4KB (same as RocksDB), configurable
- **Restart interval**: 16 keys per RocksDB convention
- **Bloom filter**: Use XXH3-64 hash (via `xxhash-rust` crate)
- **Delta encoding**: Use restart points, not continuous delta
- **CRC32C**: Use `crc32c` crate for checksums
- **Compression**: Start with LZ4, add ZSTD later
- **Index block**: One entry per data block, stores largest key

## Status

**Not started** - Depends on key encoding design from MemTable.
