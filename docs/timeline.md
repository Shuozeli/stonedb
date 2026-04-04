# StoneDB Timeline System Design

<!-- agent-updated: 2026-04-04T00:00:00Z -->

## Overview

Timeline records all internal events (SkipList ops, MemTable ops, compactions, etc.) to an append-only JSONL file. Inspired by League of Legends replay system.

**Key Design Decisions:**
- **JSONL format** - Human-readable, easy to debug, parser comes later
- **Compile-time flag** - Zero overhead when disabled (`#[cfg(timeline)]`)
- **Append-only file** - Simple, safe writes
- **Internal events** - Records SkipList, MemTable internals, not just public API

## Goals

1. **Zero-cost abstraction** - When `timeline` feature is disabled, zero runtime overhead
2. **Production-safe** - Enabled via compile-time feature flag, not runtime config
3. **Human-readable** - JSONL format easy to inspect and debug
4. **Portable** - Single file format, build parser later if needed

## File Format

### JSONL (JSON Lines)

Each line is a valid JSON object:

```jsonl
{"ts":1704067200000000,"thread":"main","seq":1,"event":"skiplist_insert","key":"dGVzdA==","value":"dmFsdWU=","level":3,"result":"ok"}
{"ts":1704067200001000,"thread":"main","seq":2,"event":"memtable_put","key":"dGVzdA==","value":"dmFsdWU=","seq":42,"size":15}
{"ts":1704067200002000,"thread":"worker-0","seq":3,"event":"compaction_start","level":0,"input_files":3}
```

| Field | Type | Description |
|-------|------|-------------|
| `ts` | u64 | Unix timestamp in microseconds |
| `thread` | string | Thread name (e.g., "main", "worker-0") |
| `seq` | u64 | Global sequence number for ordering |
| `event` | string | Event type (see below) |
| `*` | any | Event-specific payload |

## Event System Architecture

StoneDB has two complementary event mechanisms:

### 1. Timeline (Debug/Replay)
Records every internal operation to JSONL file for debugging.

### 2. Observable (Async via Tokio channel)
Notify external code of high-level events via async channel (flush, compaction, etc.).

**stonedb is async-first - uses Tokio runtime.**

## Event Types

### EventListener Events (Callbacks)

These are fire-and-forget notifications for external listeners:

| Event | Fields | Description |
|-------|--------|-------------|
| `on_flush_begin` | `memtable_id` | MemTable flush started |
| `on_flush_end` | `memtable_id`, `output_files` | MemTable flush completed |
| `on_compaction_begin` | `job_id`, `input_files`, `output_level` | Compaction started |
| `on_compaction_end` | `job_id`, `new_files`, `deleted_files` | Compaction completed |
| `on_table_file_deleted` | `file_number` | SSTable file deleted |

### Timeline Events (Recorded)

These are recorded to JSONL for replay:

### SkipList Events

| Event | Fields | Description |
|-------|--------|-------------|
| `skiplist_insert` | `key`, `value`, `level`, `result` | Insert operation |
| `skiplist_get` | `key`, `found`, `value` | Get operation |
| `skiplist_delete` | `key`, `result` | Delete operation |
| `skiplist_contains` | `key`, `found` | Contains check |
| `skiplist_lower_bound` | `key`, `found_key`, `found_value` | Lower bound search |

### MemTable Events

| Event | Fields | Description |
|-------|--------|-------------|
| `memtable_put` | `key`, `value`, `seq`, `size` | Put operation |
| `memtable_delete` | `key`, `seq`, `size` | Delete (tombstone) operation |
| `memtable_get` | `key`, `found`, `seq`, `value` | Get operation |
| `memtable_contains` | `key`, `found` | Contains check |

### Compaction Events

| Event | Fields | Description |
|-------|--------|-------------|
| `compaction_start` | `job_id`, `level`, `input_files` | Compaction started |
| `compaction_end` | `job_id`, `entries_merged`, `entries_dropped`, `duration_ms` | Compaction completed |

### SSTable Events

| Event | Fields | Description |
|-------|--------|-------------|
| `sst_write` | `file`, `entries`, `size_bytes` | SSTable flushed |
| `sst_read` | `file`, `key`, `found` | SSTable lookup |

### InternalKey Events

| Event | Fields | Description |
|-------|--------|-------------|
| `internal_key_new` | `user_key`, `seq`, `value_type` | InternalKey created |
| `internal_key_decode` | `user_key`, `seq`, `value_type` | InternalKey decoded |

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Application Code (Async)                   │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐               │
│  │ SkipList │  │ MemTable │  │Compaction│  ...          │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘               │
│       │              │              │                      │
│       └──────────────┼──────────────┘                      │
│                      ▼                                     │
│              ┌───────────────┐                            │
│              │ observable!() │   #[cfg(timeline)]         │
│              │  async emit  │                            │
│              └───────┬───────┘                            │
└──────────────────────┼────────────────────────────────────┘
                       ▼
              ┌─────────────────┐
              │  Tokio Channel  │  (mpsc::channel)
              │  buffer=10000   │
              └────────┬────────┘
                       ▼
              ┌─────────────────┐
              │ Background Task │  (tokio::spawn)
              │ TimelineWriter  │  writes to .timeline
              └────────┬────────┘
                       ▼
              ┌─────────────────┐
              │  .timeline     │
              │  (JSONL file)  │
              └─────────────────┘
```

## Observable API (Async via Tokio)

Uses Tokio's `mpsc::channel` for buffered, async-native event streaming.

```rust
use tokio::sync::mpsc;

/// Observable - produces events via buffered channel
pub struct Observable<T: Send> {
    sender: mpsc::Sender<T>,
}

impl<T: Send + 'static> Observable<T> {
    /// Create a new Observable with specified buffer size
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<T>) {
        let (sender, receiver) = mpsc::channel(buffer);
        (Self { sender }, receiver)
    }

    /// Emit an event (async, fire-and-forget if buffer full)
    pub async fn emit(&self, event: T) {
        let _ = self.sender.send(event).await;
    }

    /// Try to emit without waiting
    pub fn try_emit(&self, event: T) -> bool {
        self.sender.try_send(event).is_ok()
    }
}

/// TimelineEvent types for Observable
#[derive(Debug, Clone)]
pub enum TimelineEvent {
    // SkipList events
    SkipListInsert { key: Vec<u8>, value: Vec<u8>, level: usize },
    SkipListGet { key: Vec<u8>, found: bool },
    SkipListContains { key: Vec<u8>, found: bool },

    // MemTable events
    MemTablePut { key: Vec<u8>, seq: u64 },
    MemTableDelete { key: Vec<u8>, seq: u64 },
    MemTableGet { key: Vec<u8>, found: bool },

    // Compaction events
    CompactionBegin { job_id: u64, level: usize },
    CompactionEnd { job_id: u64, entries_merged: u64 },

    // Flush events
    FlushBegin { memtable_id: u64 },
    FlushEnd { memtable_id: u64, output_files: Vec<u64> },
}
```

## API Design

### Feature Flag

```toml
# Cargo.toml
[features]
default = []
timeline = []
```

### TimelineWriter

```rust
pub struct TimelineWriter {
    file: BufWriter<File>,
    seq: AtomicU64,
    thread_name: String,
}

impl TimelineWriter {
    pub fn new(path: &Path) -> Result<Self>;

    pub fn event(&mut self, event_type: &str, payload: &impl Serialize);

    pub fn flush(&mut self) -> Result<()>;
}
```

### Event Macro

```rust
#[cfg(timeline)]
macro_rules! event {
    ($writer:expr, $event:expr, $payload:expr) => {
        $writer.event($event, &$payload);
    };
}

#[cfg(not(timeline))]
macro_rules! event {
    ($writer:expr, $event:expr, $payload:expr) => {};
}
```

### Global Timeline Instance

```rust
#[cfg(timeline)]
lazy_static::lazy_static! {
    static ref TIMELINE: std::sync::Mutex<TimelineWriter> = {
        let path = std::env::var("STONEDB_TIMELINE")
            .unwrap_or_else(|_| ".stonedb.timeline".to_string());
        TimelineWriter::new(path).unwrap()
    };
}

#[cfg(not(timeline))]
struct TimelinePlaceholder;

#[cfg(not(timeline))]
impl TimelinePlaceholder {
    pub fn event(&self, _: &str, _: &impl serde::Serialize) {}
}
```

### Usage Example

```rust
#[cfg(timeline)]
fn insert(&mut self, key: K, value: V) -> Option<V> {
    let result = self.do_insert(key.clone(), value.clone(), top_level);
    event!(TIMELINE.lock().unwrap(), "skiplist_insert", InsertEvent {
        key: base64_encode(&key),
        value: result.clone().map(base64_encode),
        level: top_level,
        result: if result.is_some() { "updated" } else { "inserted" },
    });
    result
}

#[cfg(not(timeline))]
fn insert(&mut self, key: K, value: V) -> Option<V> {
    self.do_insert(key, value, top_level)
}
```

## Implementation Plan

### Phase 1: Core Infrastructure (Async-first with Tokio)
- [ ] Add `tokio` dependency to stonedb-core
- [ ] Create `crates/stonedb-core/src/timeline.rs`
- [ ] Implement `Observable<T>` using `tokio::sync::mpsc::channel`
- [ ] Add `event!()` async macro
- [ ] Add `timeline` feature to `Cargo.toml`

### Phase 2: SkipList Integration
- [ ] Instrument `SkipList::insert()` - emit event
- [ ] Instrument `SkipList::get()` - emit event
- [ ] Instrument `SkipList::contains()` - emit event
- [ ] Instrument `SkipList::lower_bound()` - emit event

### Phase 3: MemTable Integration
- [ ] Instrument `MemTable::put()` - emit event
- [ ] Instrument `MemTable::delete()` - emit event
- [ ] Instrument `MemTable::get()` - emit event
- [ ] Instrument `MemTable::contains()` - emit event

### Phase 4: Timeline Writer (Background Task)
- [ ] Create background task to consume Observable channel
- [ ] Write events to `.timeline` JSONL file
- [ ] Handle backpressure (buffer overflow gracefully)

### Phase 5: Compaction Integration (future)
- [ ] Instrument compaction start/end events
- [ ] Instrument SSTable flush events

### Phase 6: Testing & Polish
- [ ] Add unit tests
- [ ] Test with feature disabled (verify zero overhead)
- [ ] Document file format

## Open Questions

1. **Rotation** - Should we rotate timeline files? (e.g., daily or size-based)
2. **Compression** - Should we support gzip compression?
3. **Sampling** - Should we support sampling (e.g., record 1 in 1000 events)?
4. **Thread safety** - Should `TimelineWriter` use mutex or channel?
5. **Parser** - When do we build the JSONL parser?

## Alternatives Considered

### Binary Format (FlatBuffers/Protobuf)
- Pros: Faster parsing, smaller files
- Cons: Harder to debug, requires schema/parser

### Ring Buffer in Memory
- Pros: No I/O overhead
- Cons: Data lost on crash, no persistence

### Runtime Feature Flag
- Pros: Can toggle without recompile
- Cons: Runtime overhead, code complexity

## Status

**Not started** - Design complete, implementation follows.
