# Levels Manager

## Purpose

The Levels Manager organizes SSTables into hierarchical levels, enabling efficient storage management and guiding compaction decisions.

## RocksDB Reference

| File | Purpose |
|------|---------|
| `rocksdb/db/version_set.h` | Version, VersionEdit, compaction score |
| `rocksdb/db/version_edit.h` | VersionEdit for manifest updates |
| `rocksdb/db/column_family.h` | Column family data, SuperVersion |
| `rocksdb/db/version_storage_info.h` | Level metadata, file tracking |
| `rocksdb/db/compaction/compaction_picker_level.cc` | Level compaction picking |
| `rocksdb/include/rocksdb/column_family.h` | Column family options |

**Key line references:**
- CompactionScore computation: `version_set.cc:ComputeCompactionScore`
- Level file picking: `compaction_picker_level.cc:207-339`
- VersionEdit structure: `version_edit.h:693-1113`
- Level metadata: `version_storage_info.h`

## Core Principles

1. **Level hierarchy**: Each level is 10x larger than the previous
2. **Non-overlapping**: L1+ levels have non-overlapping key ranges
3. **Immutable files**: SSTables are never modified after creation
4. **Level metadata**: Track file boundaries and sizes per level

## RocksDB Level Structure

### Level Size Configuration

```cpp
// rocksdb/include/rocksdb/column_family.h:200-220
struct LevelCompactionOptions {
  std::vector<int> level_multiplier;  // Default: 10
  // Example: L1 = 64MB, L2 = 640MB, L3 = 6.4GB
};
```

### Default Size Ratios

```
┌────────────────────────────────────────────────────────────────────┐
│                         LSM Tree Levels                             │
├────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  L0 (64 MB default)                                                │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐                               │
│  │SSTable 1│ │SSTable 2│ │SSTable 3│  ◄── Can overlap            │
│  └─────────┘ └─────────┘ └─────────┘                               │
│                                                                     │
│  L1 (640 MB) = L0 * 10                                             │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐     │
│  │ [a-f]   │ │ [f-k]   │ │ [k-p]   │ │ [p-s]   │ │ [s-z]   │ ◄── No overlaps
│  └─────────┘ └─────────┘ └─────────┘ └─────────┘ └─────────┘     │
│                                                                     │
│  L2 (6.4 GB) = L1 * 10                                             │
│  ┌─────────────────┐ ┌─────────────────┐ ┌─────────────────┐       │
│  │    [a-g]        │ │    [g-n]       │ │    [n-z]       │       │
│  └─────────────────┘ └─────────────────┘ └─────────────────┘       │
│                                                                     │
└────────────────────────────────────────────────────────────────────┘
```

## RocksDB Version System

### Version and SuperVersion

```cpp
// rocksdb/db/column_family.h
// Version: A snapshot of the LSM tree state (files per level)
// SuperVersion: Refcount wrapper for Version, used by iterators

class ColumnFamilyData {
  std::atomic<SuperVersion*> super_version_;
  Version* current_version();  // Latest LSM tree state
};

// SuperVersion lives until all iterators close
// Allows non-blocking version switches
```

### VersionEdit (Manifest Updates)

```cpp
// rocksdb/db/version_edit.h:693-1113
class VersionEdit {
  // Deleted files
  DeletedFiles deleted_files_;  // set<pair<int, uint64_t>>

  // New files added
  NewFiles new_files_;  // vector<pair<int, FileMetaData>>

  // WAL tracking
  WalAdditions wal_additions_;
  WalDeletion wal_deletion_;
};
```

## StoneDB Levels Manager Design

### LevelHandler Structure

```rust
pub struct LevelHandler {
    pub level: usize,
    pub files: Vec<TableFile>,
}

pub struct TableFile {
    pub file_number: u64,
    pub file_path: PathBuf,
    pub smallest_key: InternalKey,
    pub largest_key: InternalKey,
    pub file_size: u64,
    pub num_entries: u64,
    // Compaction state
    pub being_compacted: AtomicBool,
}

pub struct LevelsController {
    pub levels: Vec<RwLock<LevelHandler>>,  // L0, L1, L2, ...
    pub opts: LeveledOptions,
    next_file_number: AtomicU64,
}
```

### LevelOptions

```rust
pub struct LeveledOptions {
    pub num_levels: usize,           // Default: 7
    pub max_bytes_for_level: u64,  // Target size per level
    pub max_bytes_for_level_multiplier: f64,  // Default: 10.0
    pub level0_file_num_compaction_trigger: usize,  // Default: 4
}

impl LeveledOptions {
    pub fn max_bytes_for_level(&self, level: usize) -> u64 {
        let base = 64 * 1024 * 1024;  // 64 MB base for L1
        (base as f64 * self.max_bytes_for_level_multiplier.powi(level as i32)) as u64
    }
}
```

## Key Operations

### Get (Point Lookup)

```rust
impl LevelsController {
    pub fn get(&self, key: &[u8]) -> Result<Option<Bytes>> {
        // 1. Check MemTable list (handled by DB layer)
        // 2. Check L0 SSTables (most recent first, all must be checked)
        for table in &self.levels[0].read().files {
            if let Some(v) = table.get(key)? {
                return Ok(Some(v));
            }
        }

        // 3. Check L1+ SSTables (binary search by key range)
        for level in 1..self.levels.len() {
            let handler = self.levels[level].read();
            if let Some(table) = self.find_overlapping_table(&handler, key) {
                if let Some(v) = table.get(key)? {
                    return Ok(Some(v));
                }
            }
        }

        Ok(None)
    }

    fn find_overlapping_table(&self, handler: &LevelHandler, key: &[u8]) -> Option<&Table> {
        // Binary search by key range
        // L1+ files are sorted and non-overlapping
        handler.files.binary_search_by(|table| {
            if key < table.smallest_key.as_bytes() {
                Ordering::Greater
            } else if key > table.largest_key.as_bytes() {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        }).ok().map(|i| &handler.files[i])
    }
}
```

### Get Overlapping Inputs (For Compaction)

```rust
impl LevelsController {
    /// Find all SSTables that overlap the given key range
    pub fn get_overlapping_inputs(
        &self,
        level: usize,
        start_key: &[u8],
        end_key: &[u8],
    ) -> Vec<TableFile> {
        let handler = self.levels[level].read();

        if level == 0 {
            // L0: all files can overlap, return all
            handler.files.clone()
        } else {
            // L1+: binary search, then extend to cover overlaps
            let mut result = Vec::new();
            // ... find and extend to cover all overlapping ranges
            result
        }
    }
}
```

## Compaction Score

RocksDB computes compaction scores (`version_set.cc`):

```cpp
// L0 score = num_l0_files / level0_file_num_compaction_trigger
// L1+ score = level_size / max_bytes_for_level

// When score >= 1, compaction is triggered
bool NeedsCompaction(...) {
  for (int i = 0; i <= vstorage->MaxInputLevel(); i++) {
    if (vstorage->CompactionScore(i) >= 1) {
      return true;
    }
  }
}
```

### StoneDB Compaction Score Implementation

```rust
impl LevelsController {
    pub fn needs_compaction(&self) -> bool {
        // Check L0 file count
        let l0_handler = self.levels[0].read();
        if l0_handler.files.len() >= self.opts.level0_file_num_compaction_trigger {
            return true;
        }

        // Check level sizes
        for level in 1..self.levels.len() {
            let handler = self.levels[level].read();
            let total_size: u64 = handler.files.iter().map(|f| f.file_size).sum();
            let max_size = self.opts.max_bytes_for_level(level);
            if total_size >= max_size {
                return true;
            }
        }

        false
    }

    pub fn compaction_score(&self, level: usize) -> f64 {
        if level == 0 {
            let handler = self.levels[0].read();
            handler.files.len() as f64 / self.opts.level0_file_num_compaction_trigger as f64
        } else {
            let handler = self.levels[level].read();
            let total_size: u64 = handler.files.iter().map(|f| f.file_size).sum();
            let max_size = self.opts.max_bytes_for_level(level);
            total_size as f64 / max_size as f64
        }
    }
}
```

## Connection to Compaction

```
Compaction Picker
       │
       │ "L0 has too many files"
       ▼
LevelsController.get_overlapping_inputs(L0, all_files)
       │
       │ "Find L1 files that overlap"
       ▼
LevelsController.get_overlapping_inputs(L1, key_range_of_L0_files)
       │
       ▼
CompactionExecutor
       │
       │ "Write new SSTables to L1"
       ▼
LevelsController.add_files(L1, new_sstables)      // New files
LevelsController.delete_files(L0, compacted_files)   // Old L0
LevelsController.delete_files(L1, old_sstables)    // Old L1
       │
       ▼
Manifest.log_version_edit(change)
```

## File Number Assignment

```rust
impl LevelsController {
    pub fn next_file_number(&self) -> u64 {
        self.next_file_number.fetch_add(1, Ordering::AcqRel)
    }

    pub fn new_sst_filename(&self, level: usize) -> String {
        let num = self.next_file_number();
        format!("{:06}.sst", num)  // e.g., 000001.sst
    }
}
```

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `levels/mod.rs` | LevelsController | `version_set.h/cc` |
| `levels/handler.rs` | LevelHandler | `version_storage_info.h` |
| `levels/file.rs` | TableFile structure | `column_family.h` |
| `levels/score.rs` | Compaction scoring | `version_set.cc` |

## Implementation Notes

- **L0 is special**: Files can overlap (sorted by time, not key range)
- **L1+ are sorted**: Non-overlapping key ranges, binary searchable
- **File numbers**: Unique per SSTable, monotonically increasing
- **Compaction score**: Triggers when >= 1.0
- **Version switching**: Non-blocking for readers (SuperVersion pattern)

## Status

**Not started** - Depends on SSTable implementation.
