<!-- agent-updated: 2026-05-30T20:14:59Z -->

# SkipList Across Redis, AgateDB, and RocksDB (and what StoneDB took from it)

**Date:** 2026-05-30
**Level:** Advanced
**Prerequisites:** skiplists, atomics / memory ordering, LSM-tree basics
**See also:** [skiplist-implementation-guide.md](./skiplist-implementation-guide.md),
[../architecture.md](../architecture.md)

---

## Why this doc exists

We studied three production skiplists to decide how StoneDB's MemTable should
behave, then implemented the AgateDB approach. The single most important lesson:

> A skiplist's implementation is not decided by the algorithm. It is decided by
> **three orthogonal questions** about the workload:
> 1. **Concurrency model** — who reads/writes concurrently?
> 2. **Deletion** — do individual entries get removed, or is the whole structure discarded?
> 3. **Rank** — do you need "the N-th element" / range-by-index?
>
> Answer those three and the data structure (arena vs malloc, immutable vs
> mutable nodes, span vs no span) falls out almost mechanically.

---

## Table of contents

1. [The core invariant: immutable nodes](#the-core-invariant)
2. [Three-way comparison](#three-way-comparison)
3. [Why this design is fast: 7 mechanisms](#why-fast)
4. [Performance scorecard: AgateDB vs RocksDB](#scorecard)
5. [The StoneDB bug, and the fix we shipped](#stonedb)
6. [Reference file map](#refs)

---

<a name="the-core-invariant"></a>
## 1. The core invariant: immutable nodes

In a **concurrent** arena skiplist (single-writer + concurrent readers, or
multi-writer), a node is published to readers via a release-store of its offset
into a predecessor's forward pointer. After that store, any reader may be
dereferencing the node at any time.

Therefore: **a node must never be mutated after it is linked.** Overwriting a
node's `value` in place is a data race — a reader can observe a torn value or a
freed buffer (use-after-free / double-free). This is undefined behavior, not just
a logic bug.

Both AgateDB and RocksDB obey this. Their skiplist `put` is **insert-only**: if
the key already exists, they return a conflict and do *not* overwrite. "Updates"
happen one level up, by inserting a **new versioned key** (MVCC), so the new
value is a brand-new immutable node that sorts ahead of the old one.

Redis is the instructive exception (see below): it is single-threaded, so it has
no concurrent readers and *can* legally mutate a node in place.

---

<a name="three-way-comparison"></a>
## 2. Three-way comparison

### 2.1 Redis `zskiplist` (`src/t_zset.c`, `server.h`)

Backs sorted sets (ZSET). Defining facts:

- **Single-threaded.** The whole data path is one event loop. No atomics, no CAS,
  no arena — just raw pointers and `zmalloc`.
- **Per-node malloc, flexible array member.** `zslCreateNode` allocates node +
  `level[]` + an embedded `sds` (the element string) as one block; `ZREM` frees
  nodes individually, so an arena would be wrong here.
- **Two structures.** `zset = { dict, zskiplist }`. The `dict` (element -> score)
  enforces uniqueness and O(1) lookup; the skiplist orders by `(score, element)`.
  That is why `zslInsert` may assume "the element is not already present."
- **`span` for rank.** Each level link stores a `span` (nodes crossed), giving
  O(log n) `ZRANK` / range-by-index. AgateDB and RocksDB have no rank concept.
- **In-place update is legal.** `zslUpdateScore` mutates `node->score` directly on
  its fast path (position unchanged), or unlinks + re-inserts the *same node* on
  the slow path. Safe precisely because nothing reads concurrently.
- Level params: `P = 0.25`, `MAXLEVEL = 32`.

### 2.2 AgateDB `Skiplist` (`skiplist/src/list.rs`) — the model StoneDB follows

A Rust port of Badger's skiplist (WiscKey lineage).

- **Single-writer + concurrent readers by default.** Production constructs it with
  `allow_concurrent_write = false` (`db.rs`), which means *one writer*, **not**
  one thread. Reads are always concurrent — proven by the non-concurrent write
  path still using a `Release` store to publish nodes (a release store is
  pointless without a concurrent acquiring reader). `true` enables multi-writer
  via CAS.
- **Arena + atomic offsets.** Nodes live in a pre-allocated arena; links are
  `AtomicUsize` offsets, not pointers. Allocation is one `fetch_add`. Nodes are
  never individually freed (the whole memtable is flushed, then dropped).
- **Immutable nodes + MVCC via timestamp suffix.** `put` is insert-only. Updates
  are new keys: `key_with_ts` (`format.rs`) appends 8 bytes of
  `(u64::MAX - ts).to_be()`. Inverted so a *newer* ts yields a *smaller* suffix
  and sorts first; the generic `FixedLengthSuffixComparator(8)` then "just works"
  bytewise. Reads `find_near` the newest version <= read-ts (snapshot read).
- **WiscKey value separation.** `value.rs` / `value_log.rs`: values larger than
  `value_threshold` (32B) go to a separate value-log; the LSM stores only an
  8-byte pointer (`VALUE_POINTER`). Shrinks the LSM -> less write amplification.

### 2.3 RocksDB `InlineSkipList` (`memtable/inlineskiplist.h`) — the gold standard

- Same concurrency model as AgateDB (single-writer + concurrent readers; optional
  `InsertConcurrently` with CAS), but with three extra cache/throughput tricks
  AgateDB lacks (see section 3): **inlined keys**, **cache-line node layout**, and
  the **Splice** insertion hint.
- Carefully tuned memory ordering (`Acquire`/`Release` + `NoBarrier_` variants)
  instead of blanket `SeqCst`.
- Also insert-only; updates are versioned keys (`seq << 8 | type` trailer, sorted
  so newest is first). Same immutability invariant.

### 2.4 Summary table

| | Concurrency | "Update" mechanism | In-place node mutation? |
|---|---|---|---|
| **Redis** | Single-threaded | `zslUpdateScore` in-place / unlink+reinsert | Yes — safe (no concurrent readers) |
| **AgateDB** | Single-writer + concurrent readers (CAS multi-writer optional) | New versioned key (inverted-ts suffix, MVCC) | No — nodes immutable |
| **RocksDB** | Same as AgateDB | New versioned key (`seq\|type` trailer, MVCC) | No — nodes immutable |

The dividing line for "may I mutate a node?" is **concurrent readers**, not
concurrent writers.

---

<a name="why-fast"></a>
## 3. Why this design is fast: 7 mechanisms

The skiplist algorithm is not inherently faster than a balanced tree. The speed
comes from engineering that targets **memory access**, not comparison count.

1. **Arena bump allocation.** Allocation = one `fetch_add`; no malloc lock /
   free-list / fragmentation. Memtable never frees individual nodes. Bonus:
   spatial locality.
2. **Variable-height allocation.** With `P = 1/4`, average height ~1.33, so most
   nodes are tiny — allocate only `height` pointers, not `MAX_HEIGHT`.
3. **Inlined keys (RocksDB).** The key bytes live *inside* the node block
   (`Key() = &next_[1]`), so a comparison during search touches no extra cache
   line. This is the single biggest cache win and the reason it's called
   *Inline*SkipList.
4. **Cache-line node layout (RocksDB).** Higher-level forward pointers are stored
   *before* the node (`next_[-n]`), so the hot `next_[0]` and the key sit on the
   same cache line. ~99% of traversal steps touch only those.
5. **Lock-free reads, minimal barriers.** Readers never lock. Writer publishes
   with a release store; reader uses an acquire load. RocksDB further uses
   `NoBarrier_` where a node isn't visible yet.
6. **Splice / insertion hint (RocksDB).** Memtable writes are often near-sorted;
   caching the last `prev[]/next[]` makes inserts amortized O(1). CAS failure
   recomputes only the affected level.
7. **Probabilistic balance, no rotations.** Inserts are a few local pointer swaps;
   no global restructuring. This is *why* a concurrent skiplist is an order of
   magnitude easier to get right than a concurrent B-tree — the real reason LSM
   memtables pick skiplists.

---

<a name="scorecard"></a>
## 4. Performance scorecard: AgateDB vs RocksDB

| Mechanism | RocksDB | AgateDB | Evidence |
|---|---|---|---|
| Arena bump | yes (block, never moves) | yes (but **growable**: realloc+copy) | `arena.rs` grows |
| Variable height | yes | yes | `list.rs` `size - not_used` |
| **Inlined keys** | yes | **no** (`key: Bytes`, pointer hop) | `list.rs:24` |
| **Cache-line layout** | yes | **no** (plain `[AtomicUsize; MAX]`) | `list.rs:29` |
| Lock-free reads | tuned acquire/release + NoBarrier | yes, but blanket **SeqCst** | `list.rs:50` |
| **Splice hint** | yes | **no** (full search each put) | `list.rs:198` |
| No rebalancing | yes | yes | — |

**Reading:** AgateDB adopts 4 of the 7 (arena, variable height, lock-free reads,
no rebalancing) and skips the 3 most cache-focused ones, plus uses heavier SeqCst.
So per-operation, AgateDB's skiplist is somewhat slower than RocksDB's.

**But AgateDB competes on a different axis: WiscKey value separation.** By keeping
large values out of the LSM, it shrinks the tree, cutting write amplification and
making memtable entries smaller/denser. RocksDB squeezes the in-LSM path; AgateDB
reduces how much is in the LSM at all. Different bets, both valid.

`Bytes` is also a deliberate trade: a pointer hop per compare (slower search) in
exchange for zero-copy sharing of key/value buffers across WAL -> memtable ->
flush (no memcpy).

---

<a name="stonedb"></a>
## 5. The StoneDB bug, and the fix we shipped

### The bug

StoneDB's skiplist was ported from AgateDB but introduced a deviation: `put`
overwrote a node's `value` **in place** on a duplicate key
(`(*node_ptr).value = value.clone()`). Because the skiplist supports concurrent
readers, this is the exact data race section 1 forbids — a reader cloning the
`Bytes` while a writer overwrites it can hit a torn read / use-after-free. The
existing tests missed it because none combined "reader on key K" with "writer
overwriting K's value."

Root cause: the MemTable stored **raw user keys** and leaned on the unsafe
in-place update to make "update" work — instead of using versioned keys. The
`InternalKey` encoding and `FixedLengthSuffixComparator` needed to do it the
AgateDB way already existed but were unused.

### The fix (AgateDB's way)

1. **Immutable nodes.** Deleted the in-place value overwrite in
   `skiplist/mod.rs`. `put` is now insert-only (a duplicate key returns
   `Some((key, value))`), matching AgateDB and the skiplist's own doc comment.
   Also removed a redundant, racy trailing `prev` write under concurrent insert.
2. **Versioned keys / MVCC.** Added `InternalKeyComparator` (`key.rs`) — orders by
   user key ascending, then the 8-byte `seq|type` trailer descending (newest
   first). This is StoneDB's analogue of AgateDB's inverted-ts suffix comparator.
   The MemTable's skiplist now uses it; every `put`/`delete` inserts a distinct
   immutable `InternalKey` node. Reads `seek` to `InternalKey::new_max(user_key)`
   and take the newest version (tombstone -> `None`).
3. **Latent bug found en route.** `find_near` / `find_last` returned raw
   `*const Node` pointers, but `IterRef` addresses nodes by **arena offset**.
   Nothing had ever called `seek`, so the new MemTable `lookup` was the first
   caller and it segfaulted. Made both return offsets consistently.

### Why StoneDB's `seq|type` trailer doesn't need inversion

AgateDB inverts ts so a *generic* bytewise-ascending suffix comparator yields
newest-first. StoneDB instead keeps the raw `seq|type` trailer and uses a
*custom* comparator (`InternalKeyComparator`) that compares the trailer
**descending**. Same result (newest first), different bookkeeping. Either is fine;
the custom comparator reuses the existing `InternalKey` encoding and its `Ord`.

### What StoneDB still hasn't adopted

From RocksDB: inlined keys (StoneDB stores `Bytes`, one pointer hop per compare),
the cache-line node layout, and the Splice hint. From AgateDB: WiscKey value
separation. These are future performance work, not correctness.

---

<a name="refs"></a>
## 6. Reference file map

Upstream clones live under `~/projects/thirdparty/` (Redis, RocksDB) and
`~/projects/scratch/db/agatedb`.

| Concept | Redis | AgateDB | RocksDB |
|---|---|---|---|
| Node / struct | `server.h` `zskiplistNode` | `skiplist/src/list.rs` `Node` | `memtable/inlineskiplist.h` `Node` |
| Insert | `t_zset.c` `zslInsertNode` | `list.rs` `put` | `inlineskiplist.h` `Insert<UseCAS>` |
| Update | `t_zset.c` `zslUpdateScore` | (none — versioned key) | (none — versioned key) |
| Random height | `zslRandomLevel` | `random_height` | `RandomHeight` |
| Versioned key | n/a | `format.rs` `key_with_ts` | internal key trailer |
| Value separation | n/a | `value_log.rs` | n/a (inline) |

StoneDB equivalents: `crates/stonedb-core/src/skiplist/{mod,arena,key}.rs`,
`memtable.rs`, `key.rs` (`InternalKey`, `InternalKeyComparator`).
