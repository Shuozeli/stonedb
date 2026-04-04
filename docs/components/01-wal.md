# Write-Ahead Log (WAL)

## Purpose

The WAL provides **durability** for writes. Before any data is considered committed, it must be persisted to the WAL. On crash recovery, the WAL is replayed to restore uncommitted state.

## RocksDB Reference

RocksDB WAL implementation is the primary reference:

| File | Purpose |
|------|---------|
| `rocksdb/db/log_format.h` | Block size (32KB), record types, header format |
| `rocksdb/db/log_writer.cc/h` | WAL writing, record fragmentation |
| `rocksdb/db/log_reader.cc/h` | WAL reading, record reassembly, CRC verification |
| `rocksdb/db/wal_manager.cc/h` | Multiple WAL file lifecycle |
| `rocksdb/file/writable_file_writer.cc` | fsync/fdatasync implementation |
| `rocksdb/include/rocksdb/options.h` | WALRecoveryMode enum |

**Key line references:**
- Block size: `log_format.h:54` - `constexpr unsigned int kBlockSize = 32768;`
- Header format: `log_format.h:56-61` - CRC(4) + Size(2) + Type(1)
- Record types: `log_format.h:14-23` - kFullType, kFirstType, kMiddleType, kLastType
- CRC check: `log_reader.cc:624-636`
- Sync: `writable_file_writer.cc:512-547`

## Core Principles

1. **Append-only**: Never modify existing entries, only append new ones
2. **Write-before-MemTable**: WAL write completes before data appears in MemTable
3. **Checksum protection**: Each entry has a checksum to detect corruption
4. **Block-aligned**: 32KB blocks for natural recovery points
5. **Crash recovery**: On startup, replay WAL to recover uncommitted state

## RocksDB WAL Design (Reference)

### Block-Based Format (Not Variable-Length Frames)

RocksDB uses **32KB block-aligned** WAL, not variable-length frames:

```
rocksdb/db/log_format.h:54
constexpr unsigned int kBlockSize = 32768;  // 32KB blocks

rocksdb/db/log_format.h:56-61
// Header per record: CRC(4 bytes) + Size(2 bytes) + Type(1 byte) = 7 bytes
constexpr int kHeaderSize = 4 + 2 + 1;
```

### Record Layout

```
+---------+-----------+-----------+--- ... ---+
|CRC (4B) | Size (2B) | Type (1B) | Payload   |
+---------+-----------+-----------+--- ... ---+
```

### Record Types

```cpp
// rocksdb/db/log_format.h:14-23
enum RecordType {
  kFullType = 1,
  kFirstType = 2,    // First fragment of multi-block record
  kMiddleType = 3,    // Middle fragment
  kLastType = 4,      // Last fragment
  kEof = 5,          // End of WAL file marker
  kBadRecord = 6,    // Corrupt record
  kZeroType = 7,     // Zero-filled padding (preallocated regions)
};
```

### Torn Write Protection

RocksDB handles torn writes via:

1. **CRC Checksum** (`log_reader.cc:624-636`):
```cpp
uint32_t expected_crc = crc32c::Unmask(DecodeFixed32(header));
uint32_t actual_crc = crc32c::Value(header + 6, length + header_size - 6);
if (actual_crc != expected_crc) {
  return kBadRecordChecksum;
}
```

2. **Record Fragmentation**: Records > 32KB split across blocks with kFirstType/kMiddleType/kLastType

3. **kZeroType Padding**: Preallocated zero-filled regions handled gracefully

4. **Recycler Header Verification** (`log_reader.cc:606-612`):
```cpp
if (is_recyclable_type) {
  uint32_t log_num = DecodeFixed32(header + 7);
  if (log_num != log_number_) {
    return kOldRecord;  // Stale data from previous incarnation
  }
}
```

## StoneDB WAL Design

### WAL File Format

```
┌─────────────────────────────────────────────────────────────┐
│ WAL File                                                    │
├─────────────────────────────────────────────────────────────┤
│ [ Block 0: 32KB ]                                          │
│ ┌──────────┬──────────┬──────────┬──────────┐               │
│ │ Record 1 │ Record 2 │ Record 3 │ (padding) │               │
│ └──────────┴──────────┴──────────┴──────────┘               │
├─────────────────────────────────────────────────────────────┤
│ [ Block 1: 32KB ]                                          │
│ ┌──────────┬──────────┬──────────┐                         │
│ │ Record 4 │ Record 5 │ (padding) │                          │
│ └──────────┴──────────┴──────────┘                         │
└─────────────────────────────────────────────────────────────┘
```

### WAL Entry (WriteBatch) Format

```
┌──────────────────────────────────────────────────────────────┐
│  WriteBatch Header (16 bytes)                                 │
├──────────────────────────────────────────────────────────────┤
│  ┌──────────────┬──────────────┬──────────────┬─────────────┐│
│  │ magic (4B)  │ sequence (8B) │ batch_count │ padding    ││
│  │ 0x3131...   │               │             │            ││
│  └──────────────┴──────────────┴──────────────┴─────────────┘│
├──────────────────────────────────────────────────────────────┤
│  WriteBatch Records                                           │
│  ┌────────────────────────────────────────────────────────┐ │
│  │ kTypeValue | key_len (varint) | key | value_len | value │ │
│  │ kTypeDeletion | key_len (varint) | key                   │ │
│  │ ...                                                      │ │
│  └────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

## Key Operations

### Write (Rust)

```rust
// Use POSIX I/O, NOT mmap
// rocksdb uses: write() + fsync()/fdatasync()
// See writable_file_writer.cc:512-547

pub fn write(&mut self, batch: &WriteBatch) -> Result<u64> {
    // 1. Encode batch with header + sequence
    // 2. Fragment into 32KB block-aligned records
    // 3. write() to file descriptor with O_APPEND
    // 4. fdatasync() to persist
    // 5. Return sequence number
}
```

### Sync Implementation (RocksDB Reference)

```cpp
// rocksdb/file/writable_file_writer.cc:512-547
IOStatus WritableFileWriter::SyncInternal(const IOOptions& opts, bool use_fsync) {
  if (use_fsync) {
    s = writable_file_->Fsync(opts, nullptr);   // fsync()
  } else {
    s = writable_file_->Sync(opts, nullptr);     // fdatasync()
  }
}
```

### Replay

```rust
pub fn replay<F>(&mut self, mut callback: F) -> Result<u64>
where
    F: FnMut(u64, &[u8], &[u8], EntryType),
{
    let mut block = [0u8; 32768];
    let mut block_offset = 0;

    loop {
        // Read header
        let header = self.read_header(&mut block)?;
        if header.record_type == RecordType::kEof {
            break;
        }

        // Verify CRC
        if !verify_checksum(&header) {
            // Handle based on WALRecoveryMode
            continue;
        }

        // Reassemble fragmented records
        let entry = self.reassemble_record(&header, &mut block)?;

        // Invoke callback
        callback(header.sequence, &entry.key, &entry.value, entry.entry_type)?;
    }
}
```

## WAL Recovery Modes

```rust
// rocksdb/include/rocksdb/options.h:414-431
pub enum WALRecoveryMode {
    /// Tolerate corrupted tail records (default - common after unclean shutdown)
    TolerateCorruptedTailRecords,
    /// Fail on any corruption
    AbsoluteConsistency,
    /// Stop at first corruption; all prior records valid
    PointInTimeRecovery,
    /// Skip corrupted records and continue
    SkipAnyCorruptedRecords,
}
```

## Multiple WAL Files (RocksDB Style)

RocksDB manages multiple WAL files via `WalManager`:

```cpp
// rocksdb/db/wal_manager.cc
// WAL files named: <log_number>.log
// Lifecycle:
//   1. Create new WAL when current exceeds max_total_wal_size
//   2. Archive old WAL when fully flushed to SSTable
//   3. Delete archived WALs older than recovery point
```

## StoneDB Implementation Plan

### Phase 1: Basic WAL
1. Implement block-based writer (32KB blocks, O_APPEND)
2. Implement record types (full, first, middle, last)
3. Add CRC32C checksums
4. Implement fdatasync() durability

### Phase 2: WAL Reader
1. Implement block-aligned reader
2. Implement record reassembly
3. Add CRC verification with recovery modes
4. Implement WAL replay to MemTable

### Phase 3: WAL Management
1. Multiple WAL file support
2. WAL archival and deletion
3. Integration with MemTable flush
4. MANIFEST integration for WAL tracking

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `wal/format.rs` | Block/record format | `rocksdb/db/log_format.h` |
| `wal/writer.rs` | WAL writing | `rocksdb/db/log_writer.cc` |
| `wal/reader.rs` | WAL reading | `rocksdb/db/log_reader.cc` |
| `wal/manager.rs` | WAL lifecycle | `rocksdb/db/wal_manager.cc` |
| `wal/sync.rs` | fsync/fdatasync | `rocksdb/file/writable_file_writer.cc` |

## Status

**Not started** - Foundation component, must be implemented first.

## Implementation Notes

- **Use POSIX I/O, NOT mmap** - mmap doesn't guarantee durability until msync()
- **Block-align all writes** - 32KB blocks give natural recovery points
- **Fragment large records** - Split across blocks with type markers
- **Verify CRC before processing** - Drop corrupted records based on recovery mode
- **Track log_number** - Each WAL has unique number, referenced in MANIFEST
