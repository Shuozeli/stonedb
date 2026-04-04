# StoneDB Architecture

A high-performance, gRPC + FlatBuffers-based key-value store inspired by RocksDB, written in pure Rust.

## Overview

This is an embeddable, persistent key-value database using the **LSM-tree (Log-Structured-Merge-tree)** data structure optimized for write-heavy workloads.

### Why LSM-Tree?

| B-tree | LSM-tree |
|--------|----------|
| In-place updates | Append-only |
| Random I/O on writes | Sequential I/O |
| Fast reads | Fast writes |
| Higher write amplification | Higher read amplification |

---

## High-Level Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                         Client (gRPC + FlatBuffers)               │
└──────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌──────────────────────────────────────────────────────────────────┐
│  Frontend Layer                                                   │
│  ├── Request handling (Get, Put, Delete, Scan, Batch)            │
│  ├── Transaction management                                        │
│  └── Response serialization (FlatBuffers)                         │
└──────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌──────────────────────────────────────────────────────────────────┐
│  Core DB Layer                                                    │
│  ├── DB (orchestrator) - manages the whole engine                 │
│  ├── WriteBatch - atomic multi-key operations                     │
│  └── Snapshot - point-in-time read view                          │
└──────────────────────────────────────────────────────────────────┘
                                    │
              ┌─────────────────────┼─────────────────────┐
              ▼                     ▼                     ▼
┌────────────────────┐  ┌────────────────────┐  ┌────────────────────┐
│     Write Path     │  │      Read Path     │  │    Compaction       │
│                    │  │                    │  │                    │
│  1. WriteBatch     │  │  1. MemTable       │  │  1. Compaction     │
│  2. WAL            │  │  2. L0 SSTables    │  │     Picker         │
│  3. MemTable       │  │  3. L1+ SSTables   │  │  2. Compaction     │
│  (skiplist)        │  │  (merged iterator) │  │     Executor       │
│                    │  │                    │  │  3. Manifest       │
└────────────────────┘  └────────────────────┘  │     Updates       │
                                                └────────────────────┘
                                    │
              ┌─────────────────────┴─────────────────────┐
              ▼                                           ▼
┌────────────────────┐                     ┌────────────────────┐
│   MemTable Layer   │                     │   SSTable Layer    │
│                    │                     │                    │
│  - Skiplist        │                     │  - SSTable format  │
│  - WAL (durability)│                     │  - Block builder   │
│  - Immutable list  │                     │  - Index blocks    │
│  - Flush scheduler │                     │  - Bloom filter    │
└────────────────────┘                     │  - Iterator        │
                                            └────────────────────┘
                                    │
                                    ▼
┌──────────────────────────────────────────────────────────────────┐
│  Storage Layer (File I/O)                                         │
│  ├── Memory-mapped files (mmap2)                                  │
│  ├── Posix file operations                                        │
│  ├── Directory management                                          │
│  └── File lifecycle (create, delete, rename)                      │
└──────────────────────────────────────────────────────────────────┘
```

---

## Core Components

### 1. Write-Ahead Log (WAL)
- **Purpose**: Persists writes before they're in MemTable for crash recovery
- **Format**: Append-only, frame-based with checksums (CRC32C)
- **Recovery**: Replay entries on startup to reconstruct uncommitted state

### 2. MemTable
- **Purpose**: In-memory sorted structure for fast writes
- **Implementation**: SkipList (skiplist)
- **Characteristics**:
  - O(log n) inserts and lookups
  - Lock-free reads with CAS operations
  - Becomes "immutable" when full → triggers flush

### 3. SSTable (Sorted String Table)
- **Purpose**: On-disk sorted key-value storage
- **Block size**: 4KB-16KB (configurable)
- **Components**:
  - **Data blocks** - actual KV pairs with delta encoding
  - **Index block** - maps key ranges to data blocks
  - **Bloom filter** - fast negative lookups (skip entire files)
  - **Footer** - pointers to all meta blocks

### 4. Levels Manager
- **L0**: Recently flushed tables (can have overlapping key ranges)
- **L1-Ln**: Sorted, non-overlapping files per level
- **Size ratio**: Each level is 10x larger than previous (configurable)

### 5. Compaction
- **Picker**: Decides when and what SSTables to compact
- **Executor**: Merges sorted runs, produces new SSTables, removes overwritten/deleted keys
- **Manifest**: Records all file changes atomically
- **Strategies**: Level compaction (default), Universal, FIFO

### 6. Manifest
- **Purpose**: Versioned metadata of the LSM tree state
- **Records**: Files added, files deleted, level assignments
- **Recovery**: Replay on startup to reconstruct full LSM tree state

### 7. Iterator
- **Point lookups** (Get): Check MemTable → L0 → L1+ in order
- **Range scans** (Scan): MergingIterator merges all sources in sorted order
- **Key types**: Put, Delete, SingleDelete, RangeDelete

### 8. gRPC + FlatBuffers API
- **Transport**: gRPC for reliable RPC
- **Serialization**: FlatBuffers for zero-copy serialization
- **Service**: KV operations (Put, Get, Delete, Scan, Batch, Compact)

---

## Component Dependencies

```
WAL ──────────────► MemTable ──────────► Immutable MemTable ──► SSTable Writer
  │                                              │
  │ (recovery)                                   │ (flush)
  │                                              ▼
  └─────────────────────────────────────► SSTable Files
                                                  │
                                                  ▼
                                        ┌─────────────────┐
                                        │  Levels Manager │
                                        └────────┬────────┘
                                                 │
                              ┌──────────────────┼──────────────────┐
                              ▼                  ▼                  ▼
                         ┌────────┐        ┌────────┐        ┌────────┐
                         │  L0    │   ──►  │  L1    │   ──►  │  L2+   │
                         └────────┘        └────────┘        └────────┘
                              │                               │
                              └───────────────┬───────────────┘
                                              ▼
                                    ┌─────────────────┐
                                    │   Compaction     │
                                    │   (background)   │
                                    └─────────────────┘
                                              │
                                              ▼
                                    ┌─────────────────┐
                                    │    Manifest     │
                                    │  (version edit) │
                                    └─────────────────┘
```

---

## MVP Implementation Order

### Phase 0: Workspace Setup
| Task | Delivers |
|------|----------|
| Create workspace | Clean structure |
| Create crates | stonedb-core, stonedb-storage, stonedb-engine |
| Basic test harness | Can run tests |

### Phase 1: Core Data Structures (No I/O)
| Task | Delivers |
|------|----------|
| SkipList | O(log n) sorted structure |
| MemTable | In-memory key-value store |
| InternalKey encoding | Key with sequence + type |
| Basic Iterator trait | Foundation for reads |

### Phase 2: In-Memory DB (No Persistence)
| Task | Delivers |
|------|----------|
| Put/Get/Delete | Working in-memory DB |
| MergingIterator | Correct merge semantics |
| TimelineWriter | Every operation logged |

### Phase 3: Persistence Layer
| Task | Delivers |
|------|----------|
| WAL Writer | Durability |
| WAL Reader + Replay | Crash recovery |
| SSTable Builder | Write to disk |
| SSTable Reader | Read from disk |

### Phase 4: Levels + Compaction
| Task | Delivers |
|------|----------|
| Level management | Multi-level storage |
| Compaction picker | When to compact |
| Compaction executor | Merge runs |

### Phase 5: API + Tools
| Task | Delivers |
|------|----------|
| Timeline replay tool | Replay + debug |
| gRPC API | Network access (later) |

**Key**: Get to a working in-memory DB in Phase 1-2 before adding persistence.

---

## Production Features (Post-MVP)

- **Block cache** - LRU cache for hot blocks
- **Bloom filters** - Skip entire SSTables on reads
- **Column families** - Isolated keyspaces with independent compaction
- **Transactions** - Pessimistic/optimistic locking
- **Backup/Restore** - Checkpointing support
- **Rate limiting** - Control compaction I/O
- **Compression** - Per-block (LZ4, ZSTD, Snappy)
- **Direct I/O** - Bypass OS page cache

---

## Project Structure (Workspace)

```
stonedb/
├── Cargo.toml              # Workspace root
├── Cargo.lock
│
├── crates/
│   ├── stonedb-core/      # Pure LSM engine, no I/O
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── skiplist.rs       # SkipList implementation
│   │   │   ├── memtable.rs       # MemTable (wraps SkipList)
│   │   │   ├── key.rs           # InternalKey encoding
│   │   │   ├── entry.rs         # Entry types
│   │   │   └── error.rs         # Core errors
│   │   └── tests/
│   │
│   ├── stonedb-storage/   # File I/O layer
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── wal/             # Write-ahead log
│   │   │   │   ├── mod.rs
│   │   │   │   ├── writer.rs
│   │   │   │   ├── reader.rs
│   │   │   │   └── format.rs
│   │   │   ├── sstable/         # SSTable format
│   │   │   │   ├── mod.rs
│   │   │   │   ├── builder.rs
│   │   │   │   ├── reader.rs
│   │   │   │   ├── block.rs
│   │   │   │   ├── index.rs
│   │   │   │   ├── filter.rs
│   │   │   │   └── footer.rs
│   │   │   ├── manifest/         # LSM tree metadata
│   │   │   │   ├── mod.rs
│   │   │   │   ├── edit.rs
│   │   │   │   └── reader.rs
│   │   │   └── storage.rs       # File I/O utilities
│   │   └── tests/
│   │
│   ├── stonedb-engine/    # DB orchestrator
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── db.rs            # Main DB struct
│   │   │   ├── write_batch.rs
│   │   │   ├── snapshot.rs
│   │   │   ├── levels.rs       # Level management
│   │   │   ├── compaction.rs
│   │   │   └── iterator.rs     # Merging iterators
│   │   └── tests/
│   │
│   └── stonedb-tools/     # Utilities (replay, benchmarking)
│       ├── Cargo.toml
│       ├── src/
│       │   ├── lib.rs
│       │   ├── replay.rs       # Timeline replay tool
│       │   └── bench.rs        # Benchmarking tool
│       └── bin/
│           └── stonedb-cli.rs  # CLI tool
│
├── proto/                  # FlatBuffers schemas
│   ├── api.fbs
│   └── service.proto
│
├── docs/
│   ├── architecture.md
│   ├── timeline.md        # Timeline replay system
│   └── components/
│       ├── 01-wal.md
│       └── ...
│
└── README.md
```

### Design Decisions

1. **Workspace**: Separate crates for clean separation of concerns
   - `core`: No I/O dependencies, pure data structures
   - `storage`: File I/O, SSTable format
   - `engine`: Orchestration, depends on both
   - `tools`: Replay tool, benchmarks

2. **Pure Rust**: No C/C++, everything in Rust

3. **Minimal External Dependencies**: Build simple things ourselves
   - SkipList: Implement ourselves (~200 lines)
   - Bloom Filter: Implement ourselves
   - WAL: Implement ourselves
   - CRC32C: Use `crc32c` crate (intrinsics, hard to replicate)

4. **Timeline Replay**: Every operation logged for replay/recovery
   - See [timeline.md](./timeline.md) for details

---

## Key Rust Crates

We minimize dependencies. Only use crates when necessary.

| Crate | Purpose | Can We Build Ourselves? |
|-------|---------|------------------------|
| `crc32c` | Checksums (CPU intrinsics) | No - needs intrinsics |
| `thiserror` | Error handling | No - derive macro |
| `bytes` | Zero-copy byte slices | No - complex with Arc |
| `memmap2` | Memory-mapped files | Could use std File I/O initially |
| `tonic` | gRPC framework | Yes, but later |
| `flatbuffers` | Serialization | Could use prost initially |

**Build ourselves**:
- SkipList (well-defined algorithm)
- Bloom Filter (simple bit array)
- WAL format (documented)
- SSTable format (documented)
- Manifest encoding (protobuf-like, can use manual encoding)

**Philosophy**: Add dependencies only when profiling shows it's needed.

---

## References

- [RocksDB](https://github.com/facebook/rocksdb) - Original LSM-tree database
- [Agatedb](https://github.com/tikv/agatedb) - Rust LSM-tree reference
- [LevelDB](https://github.com/google/leveldb) - Original Log-StructuredDB
- [Badger](https://github.com/outcaste-io/badger) - Another Rust LSM-tree
