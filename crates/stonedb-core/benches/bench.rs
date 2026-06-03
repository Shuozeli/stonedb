//! SkipList / MemTable benchmarks.
//!
//! Designed to isolate the axes the RocksDB-style inline-key representation
//! actually moves:
//! - `insert`        : write path (per-node heap alloc + refcount elimination)
//! - `get_hit`       : search + materialize value (the headline read path)
//! - `contains_hit`  : search only; `get_hit - contains_hit` = return-copy cost
//! - `get_miss`      : full-depth search, no value copy
//! - `scan`          : seek + iterate (cursor + inline key/value access)
//! - `read_under_write` : reads under a concurrent writer (single-writer model)
//! - `memtable_put_get` : the real DB path (InternalKey + MVCC lookup seek)
//! - `memory_per_entry` : printed footprint metric (inline vs Bytes)
//!
//! Methodology: `iter_batched` keeps arena allocation + key generation untimed;
//! `StdRng::seed_from_u64` makes runs reproducible; `Throughput::Elements`
//! reports elems/sec. Param grids are trimmed for runtime/memory; scale as needed.

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use stonedb_core::{BytewiseComparator, MemTable, SkipList};

#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

const SEED: u64 = 0x5713_D0DB_5713_D0DB;

/// Upper-bound arena capacity for `n` entries of the given key/value size.
/// 256 bytes covers the fixed header plus a full-height tower.
fn arena_cap(n: usize, key_size: usize, value_size: usize) -> usize {
    n * (key_size + value_size + 256) + (1 << 20)
}

/// A deterministic random key of `size` bytes.
fn rand_key(rng: &mut StdRng, size: usize) -> Bytes {
    let mut k = vec![0u8; size];
    rng.fill(k.as_mut_slice());
    Bytes::from(k)
}

/// A deterministic sequential key of exactly `size` bytes (zero-padded).
fn seq_key(i: usize, size: usize) -> Bytes {
    let mut k = vec![b'0'; size];
    let s = i.to_string();
    let bytes = s.as_bytes();
    // Right-align the decimal into the fixed-width buffer.
    let start = size.saturating_sub(bytes.len());
    k[start..].copy_from_slice(&bytes[bytes.len().saturating_sub(size)..]);
    Bytes::from(k)
}

fn build_list(
    keys: &[Bytes],
    value: &Bytes,
    key_size: usize,
    value_size: usize,
) -> SkipList<BytewiseComparator> {
    let cap = arena_cap(keys.len(), key_size, value_size);
    let sl = SkipList::with_capacity(BytewiseComparator::new(), cap, false);
    for k in keys {
        sl.put(k.clone(), value.clone());
    }
    sl
}

// 1. insert -- fresh list, time only the inserts (arena + keys in setup).
fn bench_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("insert");
    let n = 20_000usize;
    for &key_size in &[16usize, 64, 256] {
        for &value_size in &[8usize, 256] {
            for order in ["seq", "rand"] {
                let value = Bytes::from(vec![0xCDu8; value_size]);
                let mut rng = StdRng::seed_from_u64(SEED);
                let keys: Vec<Bytes> = (0..n)
                    .map(|i| {
                        if order == "seq" {
                            seq_key(i, key_size)
                        } else {
                            rand_key(&mut rng, key_size)
                        }
                    })
                    .collect();
                let cap = arena_cap(n, key_size, value_size);

                group.throughput(Throughput::Elements(n as u64));
                let id =
                    BenchmarkId::from_parameter(format!("k{}_v{}_{}", key_size, value_size, order));
                group.bench_function(id, |b| {
                    b.iter_batched(
                        || SkipList::with_capacity(BytewiseComparator::new(), cap, false),
                        |sl| {
                            for k in &keys {
                                sl.put(k.clone(), value.clone());
                            }
                            black_box(sl.len())
                        },
                        BatchSize::SmallInput,
                    );
                });
            }
        }
    }
    group.finish();
}

const LOOKUPS: usize = 512;

// 2/3. get_hit (search + copy value) vs contains_hit (search only).
fn bench_reads(c: &mut Criterion) {
    let n = 100_000usize;
    for (name, with_value) in [("get_hit", true), ("contains_hit", false)] {
        let mut group = c.benchmark_group(name);
        for &key_size in &[16usize, 256] {
            for &value_size in &[8usize, 4096] {
                let value = Bytes::from(vec![0xCDu8; value_size]);
                let mut rng = StdRng::seed_from_u64(SEED);
                let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
                let sl = build_list(&keys, &value, key_size, value_size);
                // Deterministic lookup order over present keys.
                let probe: Vec<Bytes> = (0..LOOKUPS)
                    .map(|_| keys[rng.gen_range(0..n)].clone())
                    .collect();

                group.throughput(Throughput::Elements(LOOKUPS as u64));
                let id = BenchmarkId::from_parameter(format!("k{}_v{}", key_size, value_size));
                group.bench_function(id, |b| {
                    b.iter(|| {
                        for k in &probe {
                            if with_value {
                                black_box(sl.get(k.as_ref()));
                            } else {
                                black_box(sl.contains(k.as_ref()));
                            }
                        }
                    });
                });
            }
        }
        group.finish();
    }
}

// 4. get_miss -- absent keys, full search depth, no value copy.
fn bench_get_miss(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_miss");
    let n = 100_000usize;
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 8]);
    let mut rng = StdRng::seed_from_u64(SEED);
    let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
    let sl = build_list(&keys, &value, key_size, 8);
    let misses: Vec<Bytes> = (0..LOOKUPS).map(|_| rand_key(&mut rng, key_size)).collect();

    group.throughput(Throughput::Elements(LOOKUPS as u64));
    group.bench_function(BenchmarkId::from_parameter(n), |b| {
        b.iter(|| {
            for k in &misses {
                black_box(sl.get(k.as_ref()));
            }
        });
    });
    group.finish();
}

// 5. scan -- seek to a random key, iterate K steps reading key+value.
fn bench_scan(c: &mut Criterion) {
    let mut group = c.benchmark_group("scan");
    let n = 100_000usize;
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 64]);
    let mut rng = StdRng::seed_from_u64(SEED);
    let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
    let sl = build_list(&keys, &value, key_size, 64);
    let starts: Vec<Bytes> = (0..64).map(|_| keys[rng.gen_range(0..n)].clone()).collect();
    let k_steps = 100usize;

    group.throughput(Throughput::Elements((starts.len() * k_steps) as u64));
    group.bench_function(BenchmarkId::from_parameter(k_steps), |b| {
        b.iter(|| {
            for s in &starts {
                let mut it = sl.iter_ref();
                it.seek(s.as_ref());
                let mut steps = 0;
                while it.valid() && steps < k_steps {
                    black_box(it.key());
                    black_box(it.value());
                    it.next();
                    steps += 1;
                }
            }
        });
    });
    group.finish();
}

// 6. read_under_write -- reads while a writer inserts concurrently.
fn bench_read_under_write(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_under_write");
    let n = 100_000usize;
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 64]);
    let mut rng = StdRng::seed_from_u64(SEED);
    let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
    // Concurrent skiplist; pre-populate, leave headroom for the writer.
    let cap = arena_cap(2 * n, key_size, 64);
    let sl = Arc::new(SkipList::with_capacity(
        BytewiseComparator::new(),
        cap,
        true,
    ));
    for k in &keys {
        sl.put(k.clone(), value.clone());
    }
    let probe: Vec<Bytes> = (0..LOOKUPS)
        .map(|_| keys[rng.gen_range(0..n)].clone())
        .collect();

    group.throughput(Throughput::Elements(LOOKUPS as u64));
    group.bench_function("1writer", |b| {
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let sl = sl.clone();
            let stop = stop.clone();
            let value = value.clone();
            thread::spawn(move || {
                let mut i = n;
                while !stop.load(Ordering::Relaxed) {
                    sl.put(seq_key(i, key_size), value.clone());
                    i += 1;
                    if i >= 2 * n {
                        break; // do not overflow the arena
                    }
                }
            })
        };
        b.iter(|| {
            for k in &probe {
                black_box(sl.get(k.as_ref()));
            }
        });
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    });
    group.finish();
}

// AgateDB-style mixed read/write sweep: a foreground worker is measured while a
// background worker runs the same read/write mix. `write_frac` is writes per 10
// operations, so 0 = read-only, 10 = write-only.
fn bench_read_write_fraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("read_write_fraction");
    let n = 50_000usize;
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 64]);
    let mut rng = StdRng::seed_from_u64(SEED);
    let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
    let probe: Vec<Bytes> = (0..LOOKUPS)
        .map(|_| keys[rng.gen_range(0..n)].clone())
        .collect();

    for &write_frac in &[0usize, 1, 5, 10] {
        let unique_write_keys = 6 * n;
        let sl = Arc::new(SkipList::with_capacity(
            BytewiseComparator::new(),
            arena_cap(8 * n, key_size, 64),
            true,
        ));
        for k in &keys {
            sl.put(k.clone(), value.clone());
        }

        let next_key = Arc::new(AtomicUsize::new(n));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let sl = sl.clone();
            let stop = stop.clone();
            let next_key = next_key.clone();
            let probe = probe.clone();
            let value = value.clone();
            thread::spawn(move || {
                let mut i = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    if write_frac > 0 && i % 10 < write_frac {
                        let key_id =
                            n + next_key.fetch_add(1, Ordering::Relaxed) % unique_write_keys;
                        sl.put(seq_key(key_id, key_size), value.clone());
                    } else {
                        let k = &probe[i % probe.len()];
                        black_box(sl.get(k.as_ref()));
                    }
                    i = i.wrapping_add(1);
                }
            })
        };

        group.throughput(Throughput::Elements(LOOKUPS as u64));
        group.bench_function(
            BenchmarkId::from_parameter(format!("w{}", write_frac)),
            |b| {
                b.iter(|| {
                    for i in 0..LOOKUPS {
                        if write_frac > 0 && i % 10 < write_frac {
                            let key_id =
                                n + next_key.fetch_add(1, Ordering::Relaxed) % unique_write_keys;
                            black_box(sl.put(seq_key(key_id, key_size), value.clone()));
                        } else {
                            let k = &probe[i % probe.len()];
                            black_box(sl.get(k.as_ref()));
                        }
                    }
                });
            },
        );

        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
    }
    group.finish();
}

// Hotspot writes: many inserts into a narrow key prefix/range. Unlike AgateDB's
// uniform benchmark, this intentionally concentrates insert positions so the
// measured limit is CAS retries on shared predecessor tower slots plus global
// arena-bump contention.
fn bench_hotspot_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("hotspot_writes");
    let n = 20_000usize;
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 64]);

    for &hot_keys in &[1usize, 16, 256] {
        let unique_versions = (6 * n).max(hot_keys) / hot_keys;
        let sl = Arc::new(SkipList::with_capacity(
            BytewiseComparator::new(),
            arena_cap(8 * n, key_size, 64),
            true,
        ));
        let next_seq = Arc::new(AtomicUsize::new(0));

        group.throughput(Throughput::Elements(LOOKUPS as u64));
        group.bench_function(
            BenchmarkId::from_parameter(format!("hot{}", hot_keys)),
            |b| {
                b.iter(|| {
                    for _ in 0..LOOKUPS {
                        let seq = next_seq.fetch_add(1, Ordering::Relaxed);
                        let bucket = seq % hot_keys;
                        let mut key = seq_key(bucket, key_size).to_vec();
                        // Keep a tiny unique suffix so writes stay insert-only while
                        // still landing in the same narrow ordered region.
                        let suffix = ((seq / hot_keys) % unique_versions).to_be_bytes();
                        let tail = key.len() - suffix.len();
                        key[tail..].copy_from_slice(&suffix);
                        black_box(sl.put(Bytes::from(key), value.clone()));
                    }
                });
            },
        );
    }
    group.finish();
}

/// A key clustered into a narrow hot range: leading 8 bytes = `bucket` (so keys
/// with the same bucket sort adjacent and share predecessors), trailing 8 bytes
/// = a globally unique id (so every `put` is a real insert, not a dedup no-op).
fn hot_key(bucket: u64, unique: u64, key_size: usize) -> Bytes {
    let mut k = vec![0u8; key_size];
    k[0..8].copy_from_slice(&bucket.to_be_bytes());
    let tail = key_size - 8;
    k[tail..].copy_from_slice(&unique.to_be_bytes());
    Bytes::from(k)
}

// Hotspot write scaling: M threads concurrently insert into a narrow hot range
// (shared predecessors). The expected flat/declining throughput-vs-M curve is the
// CAS-on-shared-tower-slots + global arena-bump contention bottleneck. A single
// thread has no contention, so concurrency is what makes this meaningful.
fn bench_hotspot_write_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("hotspot_write_scaling");
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 64]);
    let per_thread = 4_000usize;
    let hot_buckets = 64u64; // narrow ordered region -> contended predecessors

    for &threads in &[1usize, 2, 4, 8] {
        let total = threads * per_thread;
        let cap = arena_cap(total + 16, key_size, 64);
        group.throughput(Throughput::Elements(total as u64));
        group.bench_function(BenchmarkId::from_parameter(threads), |b| {
            b.iter_batched(
                || {
                    let sl = Arc::new(SkipList::with_capacity(
                        BytewiseComparator::new(),
                        cap,
                        true,
                    ));
                    let batches: Vec<Vec<Bytes>> = (0..threads)
                        .map(|t| {
                            (0..per_thread)
                                .map(|j| {
                                    let global = (t * per_thread + j) as u64;
                                    hot_key(global % hot_buckets, global, key_size)
                                })
                                .collect()
                        })
                        .collect();
                    (sl, batches)
                },
                |(sl, batches)| {
                    let handles: Vec<_> = batches
                        .into_iter()
                        .map(|batch| {
                            let sl = sl.clone();
                            let value = value.clone();
                            thread::spawn(move || {
                                for k in batch {
                                    sl.put(k, value.clone());
                                }
                            })
                        })
                        .collect();
                    for h in handles {
                        h.join().unwrap();
                    }
                    black_box(sl.len())
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

// Hotspot read skew: ~95% of gets hit a 1% hot subset. Reads are lock-free loads,
// so the hot nodes stay cache-resident -- `skew95` should beat `uniform`,
// confirming reads are not the contention bottleneck.
fn bench_hotspot_read_skew(c: &mut Criterion) {
    let mut group = c.benchmark_group("hotspot_read_skew");
    let n = 100_000usize;
    let key_size = 16usize;
    let value = Bytes::from(vec![0u8; 64]);
    let mut rng = StdRng::seed_from_u64(SEED);
    let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
    let sl = build_list(&keys, &value, key_size, 64);
    let hot = (n / 100).max(1); // 1% hot subset

    for (name, skew) in [("uniform", false), ("skew95", true)] {
        let probe: Vec<Bytes> = (0..LOOKUPS)
            .map(|_| {
                let idx = if skew && rng.gen_range(0..100) < 95 {
                    rng.gen_range(0..hot)
                } else {
                    rng.gen_range(0..n)
                };
                keys[idx].clone()
            })
            .collect();

        group.throughput(Throughput::Elements(LOOKUPS as u64));
        group.bench_function(BenchmarkId::from_parameter(name), |b| {
            b.iter(|| {
                for k in &probe {
                    black_box(sl.get(k.as_ref()));
                }
            });
        });
    }
    group.finish();
}

// 7. memtable_put_get -- the real DB path (InternalKey + MVCC lookup).
fn bench_memtable(c: &mut Criterion) {
    let mut group = c.benchmark_group("memtable_put_get");
    let n = 20_000usize;
    let key_size = 16usize;
    let value = vec![0u8; 64];
    let mut rng = StdRng::seed_from_u64(SEED);
    let keys: Vec<Vec<u8>> = (0..n)
        .map(|_| {
            let mut k = vec![0u8; key_size];
            rng.fill(k.as_mut_slice());
            k
        })
        .collect();

    group.throughput(Throughput::Elements(n as u64));
    group.bench_function("put", |b| {
        b.iter_batched(
            || MemTable::with_capacity(0, arena_cap(n, key_size + 8, 64)),
            |mut m| {
                for k in &keys {
                    m.put(k, &value).unwrap();
                }
                black_box(m.len())
            },
            BatchSize::SmallInput,
        );
    });

    let mut m = MemTable::with_capacity(0, arena_cap(n, key_size + 8, 64));
    for k in &keys {
        m.put(k, &value).unwrap();
    }
    let probe: Vec<Vec<u8>> = (0..LOOKUPS)
        .map(|_| keys[rng.gen_range(0..n)].clone())
        .collect();
    group.throughput(Throughput::Elements(LOOKUPS as u64));
    group.bench_function("get", |b| {
        b.iter(|| {
            for k in &probe {
                black_box(m.get(k).unwrap());
            }
        });
    });
    group.finish();
}

// 8. memory_per_entry -- printed footprint metric (not a timed benchmark).
fn bench_memory_per_entry(c: &mut Criterion) {
    let n = 100_000usize;
    for &key_size in &[16usize, 64, 256] {
        for &value_size in &[8usize, 4096] {
            let value = Bytes::from(vec![0u8; value_size]);
            let mut rng = StdRng::seed_from_u64(SEED);
            let keys: Vec<Bytes> = (0..n).map(|_| rand_key(&mut rng, key_size)).collect();
            let sl = build_list(&keys, &value, key_size, value_size);
            let per_entry = sl.mem_size() as f64 / n as f64;
            println!(
                "memory_per_entry k{}_v{}: {:.1} bytes/entry (payload {} bytes)",
                key_size,
                value_size,
                per_entry,
                key_size + value_size
            );
        }
    }
    // Keep a trivial timed function so criterion records the group.
    c.bench_function("memory_per_entry/noop", |b| b.iter(|| black_box(1u8)));
}

criterion_group!(
    benches,
    bench_insert,
    bench_reads,
    bench_get_miss,
    bench_scan,
    bench_read_under_write,
    bench_read_write_fraction,
    bench_hotspot_writes,
    bench_hotspot_write_scaling,
    bench_hotspot_read_skew,
    bench_memtable,
    bench_memory_per_entry,
);
criterion_main!(benches);
