# Manifest

## Purpose

The Manifest records the **versioned state** of the LSM tree, enabling crash recovery and maintaining a history of changes.

## RocksDB Reference

| File | Purpose |
|------|---------|
| `rocksdb/db/version_set.h` | Version, VersionSet, LogAndApply |
| `rocksdb/db/version_edit.h` | VersionEdit structure and encoding |
| `rocksdb/db/filename.h` | File naming utilities |
| `rocksdb/db/db_impl.cc` | DB recovery logic |
| `rocksdb/include/rocksdb/column_family.h` | Column family options |

**Key line references:**
- VersionEdit structure: `version_edit.h:693-1113`
- VersionEdit tags: `version_edit.h:37-77` - kDeletedFile, kNewFile, etc.
- LogAndApply: `version_set.cc:LogAndApply()` - atomic manifest update
- Atomic group: `version_edit.h:977-985` - MarkAtomicGroup
- CURRENT file: `filename.h:78-85` - FileName::Current()

## Core Principles

1. **Append-only**: Never modify existing records
2. **Atomic updates**: State changes only applied after successful write
3. **Crash recovery**: Replay manifest to reconstruct LSM tree state
4. **Versioning**: Each state snapshot is a "Version"

## RocksDB Manifest Design

### MANIFEST File Structure

```cpp
// rocksdb/db/version_set.cc
// MANIFEST = VersionEdit records sequentially appended
// Footer at end points to last VersionEdit
```

### VersionEdit Structure

```cpp
// rocksdb/db/version_edit.h:693-1113
class VersionEdit {
  // Identity
  std::string comparator_;
  uint64_t log_number_;

  // File accounting
  uint64_t next_file_number_;
  uint64_t last_sequence_;

  // Deleted files
  DeletedFiles deleted_files_;  // set<pair<int, uint64_t>>

  // New files added
  NewFiles new_files_;  // vector<pair<int, FileMetaData>>

  // WAL tracking
  WalAdditions wal_additions_;
  WalDeletion wal_deletion_;

  // Atomic group for batched edits
  bool is_in_atomic_group_;
  uint32_t remaining_entries_;
};
```

### VersionEdit Tags (Encoding)

```cpp
// rocksdb/db/version_edit.h:37-77
enum Tag {
  kComparator = 1,
  kLogNumber = 2,
  kNextFileNumber = 3,
  kLastSequence = 4,
  kDeletedFile = 6,
  kNewFile = 7,
  kNewFile2 = 100,
  kNewFile3 = 101,
  kNewFile4 = 102,
  kInAtomicGroup = 300,
  // ... more tags
};
```

### CURRENT File

```cpp
// rocksdb/db/filename.h:78-85
// CURRENT file contains the name of the current MANIFEST
// Format: "MANIFEST-000001" or just "MANIFEST"

// rocksdb/db/filename.cc:78-85
static std::string FormatFileNumber(uint64_t num) {
  char buf[32];
  snprintf(buf, sizeof(buf), "%06llu", (unsigned long long)num);
  return buf;
}
```

## StoneDB Manifest Design

### File Structure

```
┌────────────────────────────────────────────────────────────────────┐
│                        MANIFEST File                                │
├────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │  File Header (fixed size)                                  │   │
│  │  ┌────────────────────────────────────────────────────────┐ │   │
│  │  │ magic: u32 = 0x4D414E46 ("MANF")                     │ │   │
│  │  │ version: u32                                           │ │   │
│  │  │ comparator_name: string (length-prefixed)              │ │   │
│  │  └────────────────────────────────────────────────────────┘ │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │  VersionEdit Records (sequentially appended)              │   │
│  │  ┌────────────────────────────────────────────────────────┐ │   │
│  │  │ Tag | Length | Payload                                 │ │   │
│  │  │ Tag | Length | Payload                                 │ │   │
│  │  └────────────────────────────────────────────────────────┘ │   │
│  └────────────────────────────────────────────────────────────┘   │
│                                                                     │
│  ┌────────────────────────────────────────────────────────────┐   │
│  │  Footer (48 bytes)                                         │   │
│  │  ┌────────────────────────────────────────────────────────┐ │   │
│  │  │ manifest_handle (offset, size)                        │ │   │
│  │  │ checksum                                               │ │   │
│  │  └────────────────────────────────────────────────────────┘ │   │
│  └────────────────────────────────────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────┘
```

### VersionEdit Structure (Rust)

```rust
pub struct VersionEdit {
    // Identity
    pub comparator_name: Option<String>,
    pub log_number: Option<u64>,

    // File accounting
    pub next_file_number: Option<u64>,
    pub last_sequence: Option<u64>,

    // New SSTables
    pub new_files: Vec<NewFileInfo>,

    // Deleted SSTables
    pub deleted_files: Vec<(usize, u64)>,  // (level, file_number)

    // WAL tracking
    pub wal_additions: Vec<WalInfo>,
    pub wal_deletion: Option<u64>,
}

pub struct NewFileInfo {
    pub level: usize,
    pub file_number: u64,
    pub file_size: u64,
    pub smallest_key: InternalKey,
    pub largest_key: InternalKey,
}
```

## LogAndApply (Atomic Update)

```rust
impl Manifest {
    /// Atomically write VersionEdit to MANIFEST and apply to Version
    pub fn log_and_apply(&self, edit: VersionEdit) -> Result<()> {
        // 1. Encode VersionEdit to bytes
        let encoded = edit.encode();

        // 2. Append to MANIFEST file
        self.writer.write(&encoded)?;

        // 3. Sync MANIFEST to disk
        self.writer.sync()?;

        // 4. Update in-memory state
        self.state.apply(&edit)?;

        Ok(())
    }
}

impl ManifestState {
    pub fn apply(&mut self, edit: &VersionEdit) {
        // Update next_file_number
        if let Some(n) = edit.next_file_number {
            self.next_file_number = n;
        }

        // Add new files to levels
        for new_file in &edit.new_files {
            self.levels[new_file.level].push(new_file.clone());
        }

        // Remove deleted files
        for (level, file_num) in &edit.deleted_files {
            self.levels[*level].retain(|f| f.file_number != *file_num);
        }

        // Update last_sequence
        if let Some(s) = edit.last_sequence {
            self.last_sequence = s;
        }
    }
}
```

## Recovery Process

```rust
impl Manifest {
    pub fn recover(&self) -> Result<ManifestState> {
        // 1. Find MANIFEST file (from CURRENT file)
        let current_content = read_file("CURRENT")?;
        let manifest_name = parse_manifest_name(&current_content);

        // 2. Open MANIFEST file
        let mut reader = ManifestReader::open(&manifest_name)?;

        // 3. Read and replay all VersionEdit records
        let mut state = ManifestState::default();

        loop {
            match reader.read_record()? {
                Some(record) => {
                    let edit = VersionEdit::decode(&record)?;
                    state.apply(&edit);
                }
                None => break,  // EOF
            }
        }

        // 4. Return final state
        Ok(state)
    }
}
```

### CURRENT File Management

```rust
/// Atomically switch to new MANIFEST
pub fn create_new_manifest(&self, manifest: &Manifest) -> Result<()> {
    let new_number = self.next_manifest_number();

    // 1. Write new MANIFEST with final state
    let new_manifest_path = format!("MANIFEST-{:06}", new_number);
    // ...

    // 2. Atomically update CURRENT
    let current_tmp = format!("{}/CURRENT.tmp", self.db_path);
    write_file(&current_tmp, &new_manifest_name)?;
    rename(&current_tmp, &format!("{}/CURRENT", self.db_path))?;

    // 3. Delete old MANIFEST
    if let Some(old) = self.current_manifest.take() {
        delete_file(&old)?;
    }

    Ok(())
}
```

## Log Number Tracking

The `log_number` in VersionEdit tracks which WAL files are still needed:

```cpp
// rocksdb/db/dbformat.h:1181-1240
// PredecessorWALInfo tracks WAL chain for verification
struct PredecessorWALInfo {
  uint64_t wal_number;
  uint64_t wal_size;
  uint64_t last_sequence;
};
```

**Purpose**: When a MemTable is flushed:
1. All entries from that WAL are now in SSTable
2. WAL can be deleted
3. `log_number` in VersionEdit marks this transition

## Manifest vs. WAL

| Aspect | Manifest | WAL |
|--------|----------|-----|
| Purpose | LSM tree structure | Actual data |
| Contents | File lists, metadata | Key-value entries |
| Replay | Rebuild tree structure | Rebuild MemTable |
| Size | Small | Can grow large |
| Retention | Keep current + archived | Delete after flush |

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `manifest/mod.rs` | Manifest struct | `version_set.h/cc` |
| `manifest/edit.rs` | VersionEdit structure | `version_edit.h` |
| `manifest/state.rs` | ManifestState | `version_set.cc` |
| `manifest/reader.rs` | Manifest replay | `version_set.cc:Recover()` |
| `manifest/writer.rs` | Manifest writing | `version_set.cc:LogAndApply()` |
| `manifest/filename.rs` | File naming | `filename.h` |

## Implementation Notes

- **Protobuf encoding** for VersionEdit (same as RocksDB)
- **Atomic CURRENT update** using rename
- **Manifest compaction**: When MANIFEST grows too large, write new one with full state
- **CRC checksums** on records for corruption detection
- **Atomic groups**: Group multiple edits that must be applied atomically

## Status

**Not started** - Depends on SSTable and Levels. Can be designed early but implemented later.
