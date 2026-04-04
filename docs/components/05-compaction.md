# Compaction

## Purpose

Compaction is the **background process** that:
1. Merges SSTables from one level to the next
2. Removes overwritten/deleted keys
3. Maintains level size ratios
4. Keeps the LSM tree performant

## RocksDB Reference

| File | Purpose |
|------|---------|
| `rocksdb/db/compaction/compaction_picker.h` | Base compaction picker |
| `rocksdb/db/compaction/compaction_picker_level.cc` | Level compaction picker |
| `rocksdb/db/compaction/compaction_picker_universal.cc` | Universal compaction picker |
| `rocksdb/db/compaction/compaction_picker_fifo.cc` | FIFO compaction picker |
| `rocksdb/db/compaction/compaction_job.cc` | Compaction execution, subcompaction |
| `rocksdb/db/compaction/compaction.h` | Compaction metadata |
| `rocksdb/db/compaction/compaction_outputs.cc` | Output file writing |
| `rocksdb/db/compaction/compaction_iterator.cc` | Key iteration, merge, tombstone drop |
| `rocksdb/db/version_edit.h` | VersionEdit structure |
| `rocksdb/db/version_set.h` | Version management, LogAndApply |
| `rocksdb/db/write_controller.h` | Write stall control |

**Key line references:**
- Compaction triggers: `compaction_picker_level.cc:22-48` - NeedsCompaction()
- Score computation: `version_set.cc:ComputeCompactionScore`
- Subcompaction: `compaction_job.cc:535-744` - GenSubcompactionBoundaries, RunSubcompactions
- Tombstone drop: `compaction_outputs.cc:621-629` - AddRangeDels() tombstone handling
- Write stalls: `write_controller.h:24-110` - WriteController, StopWriteToken, DelayWriteToken
- VersionEdit atomic: `version_edit.h:693-1113` - is_in_atomic_group_

## Core Principles

1. **Never blocks readers**: Compaction runs in background threads
2. **Atomic manifest updates**: LSM tree state only updated after compaction completes
3. **Level ordering**: Always compacts from level N to level N+1
4. **No data loss**: Old files kept until new files are fully written

## RocksDB Compaction Strategies

### Level Compaction (Default)

```cpp
// rocksdb/db/compaction/compaction_picker_level.cc
// L0 -> L1 -> L2 -> ... -> Ln

// Each level is 10x larger than previous
// L1 target: 64 MB, L2: 640 MB, L3: 6.4 GB, etc.

// Compaction picks:
// 1. Score >= 1 triggers compaction
// 2. Pick L0 files that overlap with L1 files
// 3. Merge and write to L1
```

### Compaction Triggers

```cpp
// rocksdb/db/compaction/compaction_picker_level.cc:22-48
bool LevelCompactionPicker::NeedsCompaction(const VersionStorageInfo* vstorage) const {
  if (!vstorage->ExpiredTtlFiles().empty()) return true;
  if (!vstorage->FilesMarkedForCompaction().empty()) return true;
  for (int i = 0; i <= vstorage->MaxInputLevel(); i++) {
    if (vstorage->CompactionScore(i) >= 1) {  // <-- Score-based trigger
      return true;
    }
  }
  return false;
}
```

### Universal Compaction

```cpp
// rocksdb/db/compaction/compaction_picker_universal.cc:767-868
// PickCompaction() order:
// 1. MaybePickPeriodicCompaction() - full compaction
// 2. MaybePickSizeAmpCompaction() - reduce size amplification
// 3. MaybePickCompactionToReduceSortedRunsBasedFileRatio()
// 4. MaybePickCompactionToReduceSortedRuns()
// Lower write amplification than level compaction
```

### FIFO Compaction

```cpp
// rocksdb/db/compaction/compaction_picker_fifo.cc:60-763
// Deletes oldest files when total size exceeds limit
// Good for time-series data with TTL
```

## StoneDB Compaction Design

### CompactionJob Structure

```rust
pub struct CompactionJob {
    pub job_id: u64,
    pub compact_type: CompactionType,
    pub input_levels: Vec<InputLevel>,
    pub output_level: usize,
    pub output_path: PathBuf,
}

pub struct InputLevel {
    pub level: usize,
    pub files: Vec<Arc<Table>>,
    pub overlapping_key_range: (InternalKey, InternalKey),
}

pub enum CompactionType {
    Level,
    Universal,
    Fifo,
}
```

### Compaction Picker

```rust
pub struct CompactionPicker {
    pub opts: LeveledOptions,
}

impl CompactionPicker {
    /// Returns (level, score) for levels needing compaction
    pub fn pick_compaction(&self, levels: &LevelsController) -> Option<(usize, f64)> {
        // Check L0 file count
        let l0_score = levels.compaction_score(0);
        if l0_score >= 1.0 {
            return Some((0, l0_score));
        }

        // Check L1+ level sizes
        for level in 1..levels.num_levels() {
            let score = levels.compaction_score(level);
            if score >= 1.0 {
                return Some((level, score));
            }
        }

        None
    }

    pub fn setup_compaction(&self, levels: &LevelsController, level: usize) -> CompactionJob {
        let mut job = CompactionJob {
            job_id: new_job_id(),
            compact_type: CompactionType::Level,
            input_levels: Vec::new(),
            output_level: level + 1,
            output_path: levels.db_path.clone(),
        };

        if level == 0 {
            // L0 compaction: all L0 files + overlapping L1 files
            let l0_files = levels.levels[0].read().files.clone();
            let key_range = compute_key_range(&l0_files);
            let l1_files = levels.get_overlapping_inputs(1, &key_range);

            job.input_levels.push(InputLevel { level: 0, files: l0_files, overlapping_key_range: key_range.clone() });
            job.input_levels.push(InputLevel { level: 1, files: l1_files, overlapping_key_range: key_range });
        } else {
            // L1+ compaction: one file + overlapping L2 files
            // ...
        }

        job
    }
}
```

## Compaction Iterator (Key Merge)

```rust
/// Merges multiple sorted inputs, outputs newest key per entry
pub struct CompactionIterator {
    inputs: Vec<Box<dyn Iterator>>,
    cmp: InternalKeyComparator,
    current_key: Bytes,
    current_value: Bytes,
    current_seq: u64,
}

impl CompactionIterator {
    pub fn next(&mut self) -> Result<Option<Entry>> {
        // 1. Find entry with smallest key from all inputs
        let mut smallest_entry: Option<Entry> = None;

        for (i, input) in self.inputs.iter_mut().enumerate() {
            if !input.valid() {
                continue;
            }

            let key = input.key();
            match &smallest_entry {
                None => {
                    smallest_entry = Some(Entry::new(key, input.value(), i));
                }
                Some(current) => {
                    if self.cmp.compare(key, &current.key) < 0 {
                        smallest_entry = Some(Entry::new(key, input.value(), i));
                    }
                }
            }
        }

        // 2. If multiple inputs have same key, keep highest sequence
        let entry = match smallest_entry {
            Some(e) => {
                self.deduplicate_with_same_key(&mut self.inputs, &e)?;
                e
            }
            None => return Ok(None),
        };

        // 3. Check if key should be skipped (tombstone drop)
        if self.should_drop_tombstone(&entry)? {
            // Skip this key, continue
            return self.next();
        }

        Ok(Some(entry))
    }

    fn deduplicate_with_same_key(&self, inputs: &mut [Box<dyn Iterator>], entry: &Entry) -> Result<()> {
        // Collect all entries with same key from all inputs
        let mut best_entry = entry.clone();

        for input in inputs.iter_mut() {
            if !input.valid() {
                continue;
            }
            if self.cmp.compare(input.key(), &best_entry.key) == Ordering::Equal {
                // Same key - keep higher sequence number
                let input_entry = Entry::new(input.key(), input.value(), 0);
                if input_entry.sequence > best_entry.sequence {
                    best_entry = input_entry;
                }
                input.next();
            }
        }

        self.current_key = best_entry.key.clone();
        self.current_value = best_entry.value.clone();
        self.current_seq = best_entry.sequence;
        Ok(())
    }
}
```

## Tombstone Handling (RocksDB Reference)

RocksDB drops tombstones when safe (`compaction_outputs.cc:621-629`):

```cpp
bool consider_drop =
    tombstone.seq_ <= earliest_snapshot &&  // No active reader
    (ts_sz == 0 ||  // Range fully covered by newer tombstones
     (!full_history_ts_low.empty() &&
      cmp->CompareTimestamp(ts, full_history_ts_low) < 0));
```

### StoneDB Tombstone Policy

```rust
impl CompactionIterator {
    fn should_drop_tombstone(&self, entry: &Entry) -> Result<bool> {
        // Only drop if:
        // 1. This is a tombstone
        // 2. No active snapshot needs it (seq > earliest_snapshot)
        // 3. Key doesn't exist in any later level

        if !is_tombstone(&entry.value) {
            return Ok(false);
        }

        let earliest = self.db.earliest_active_snapshot()?;

        // If no snapshots, tombstone is safe to drop
        if earliest == SEQUENCE_MAX {
            return Ok(true);
        }

        // Check if key has newer entry in later levels
        // This is handled by the compaction merge - only newest entry survives
        Ok(entry.sequence < earliest)
    }
}
```

## Subcompaction (Parallel Compaction)

RocksDB splits large compactions into parallel ranges (`compaction_job.cc:535-744`):

```cpp
void CompactionJob::GenSubcompactionBoundaries() {
  // Uses TableReader::ApproximateKeyAnchors() to find ~128 anchor points
  // Divides into max_subcompactions ranges with roughly equal size
  uint64_t target_range_size = total_size / num_planned_subcompactions;
}
```

### StoneDB Subcompaction

```rust
impl CompactionJob {
    pub fn run(&self) -> Result<CompactionOutput> {
        // 1. Generate subcompaction boundaries
        let boundaries = self.compute_boundaries()?;

        if boundaries.len() <= 1 {
            // Single-threaded compaction
            self.run_single_threaded()
        } else {
            // Multi-threaded subcompaction
            self.run_subcompactions(&boundaries)
        }
    }

    fn run_subcompactions(&self, boundaries: &[(InternalKey, InternalKey)]) -> Result<CompactionOutput> {
        let handles: Vec<_> = boundaries[1..].iter().zip(boundaries[..1].iter())
            .map(|(start, end)| {
                let start = start.clone();
                let end = end.clone();
                let job = self.clone();
                std::thread::spawn(move || {
                    job.run_range(&start, &end)
                })
            })
            .collect();

        // Run first range in current thread
        let mut output = self.run_range(&boundaries[0], &boundaries[1])?;

        // Collect results from threads
        for handle in handles {
            let range_output = handle.join().unwrap()?;
            output.merge(range_output);
        }

        Ok(output)
    }
}
```

## VersionEdit (Manifest Updates)

```rust
pub struct VersionEdit {
    pub comparator_name: String,
    pub log_number: Option<u64>,
    pub next_file_number: Option<u64>,
    pub last_sequence: Option<u64>,
    pub new_files: Vec<NewFileInfo>,
    pub deleted_files: Vec<(usize, u64)>,  // (level, file_number)
}

pub struct NewFileInfo {
    pub level: usize,
    pub file_number: u64,
    pub file_size: u64,
    pub smallest_key: InternalKey,
    pub largest_key: InternalKey,
}

impl VersionEdit {
    pub fn encode(&self) -> Vec<u8> {
        // Protobuf encoding
    }
}
```

### Atomic Manifest Update (RocksDB)

```cpp
// rocksdb/db/version_set.cc:LogAndApply()
// 1. Encode VersionEdit to buffer
// 2. Write to MANIFEST file
// 3. Sync MANIFEST to disk
// 4. Update in-memory Version
// All atomically
```

## Write Stalls (RocksDB Reference)

```cpp
// rocksdb/db/write_controller.h:24-110
class WriteController {
  std::atomic<int> total_stopped_;      // Stop tokens
  std::atomic<int> total_delayed_;      // Delay tokens
  std::atomic<int> total_compaction_pressure_;
};

// Stall conditions:
// - Stopped: level0_file_num_compaction_trigger * 2 L0 files
// - Delayed: L0 files > trigger, or pending bytes > 0
```

### StoneDB Write Stalls

```rust
pub enum WriteStallCondition {
    Normal,
    Delayed,   // Writes slowed
    Stopped,   // Writes blocked
}

impl WriteController {
    pub fn get_stall_condition(&self, levels: &LevelsController) -> WriteStallCondition {
        let l0_count = levels.levels[0].read().files.len();
        let trigger = levels.opts.level0_file_num_compaction_trigger;

        if l0_count >= trigger * 2 {
            return WriteStallCondition::Stopped;
        } else if l0_count > trigger {
            return WriteStallCondition::Delayed;
        }

        WriteStallCondition::Normal
    }
}
```

## Key Files

| File | Purpose | RocksDB Reference |
|------|---------|-------------------|
| `compaction/mod.rs` | CompactionJob, coordinator | `compaction_job.cc` |
| `compaction/picker.rs` | CompactionPicker | `compaction_picker_level.cc` |
| `compaction/iterator.rs` | CompactionIterator | `compaction_iterator.cc` |
| `compaction/outputs.rs` | CompactionOutputs | `compaction_outputs.cc` |
| `compaction/subcompaction.rs` | Subcompaction | `compaction_job.cc:535-744` |
| `compaction/tombstone.rs` | Tombstone policy | `compaction_outputs.cc:621-629` |

## Implementation Notes

- **Multi-threaded compaction**: Use subcompaction for parallelism
- **Tombstone drop**: Check earliest_active_snapshot() before dropping
- **VersionEdit atomic**: Write to MANIFEST, sync, then update memory
- **Write stalls**: Monitor L0 file count, slow/stop writes when needed
- **Manifest**: See component 06-manifest.md

## Status

**Not started** - Most complex component; requires all storage components first.
