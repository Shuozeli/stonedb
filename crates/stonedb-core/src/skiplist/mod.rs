//! SkipList Implementation
//!
//! Production-grade SkipList based on AgateDB's design, with RocksDB-style
//! inline key/value storage.
//! - Arena allocator (no malloc per insert)
//! - Key and value bytes stored **inline** in the arena node (no `Bytes` handle,
//!   no per-node heap allocation, no pointer hop on compare)
//! - Lock-free CAS for concurrent writes; single-writer + concurrent readers
//! - AtomicUsize for lock-free height management
//! - Reverse iteration via prev pointer (single-writer mode)
//! - Pluggable KeyComparator for custom key ordering
//!
//! References:
//! - <https://github.com/tikv/agatedb/blob/master/skiplist/src/list.rs>
//! - RocksDB `memtable/inlineskiplist.h`

mod arena;
pub use self::arena::Arena;

mod key;
pub use self::key::{BytewiseComparator, KeyComparator};

use bytes::Bytes;
use rand::Rng;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

/// Maximum height for skip list nodes.
/// With probability 1/3^19, height could reach 20.
pub const MAX_HEIGHT: usize = 20;

/// Probability of increasing height = 1/3 (u32::MAX / 3)
const HEIGHT_INCREASE: u32 = u32::MAX / 3;

// --- Node layout in the arena (single allocation, 8-byte aligned base) ---
//
//   +0    key_len:   u32
//   +4    value_len: u32
//   +8    height:    u32                  top level index; tower has height+1 entries
//   +12   (padding)
//   +16   prev:      AtomicUsize          level-0 back pointer (single-writer mode)
//   +24   tower:     AtomicUsize * (height+1)   forward offsets
//   ...   key   bytes [key_len]
//   ...   value bytes [value_len]
//
// Every AtomicUsize field is 8-byte aligned: the base is 8-aligned (arena
// guarantee) and the header is a fixed 24 bytes. key_len/value_len/height are
// 4-byte-aligned u32s. Key/value are raw bytes with no alignment requirement.

const KEY_LEN_OFF: usize = 0;
const VALUE_LEN_OFF: usize = 4;
const HEIGHT_OFF: usize = 8;
const PREV_OFF: usize = 16;
const TOWER_OFF: usize = 24;

/// Byte offset (within a node) where the inline key bytes begin.
#[inline]
fn kv_data_off(height: usize) -> usize {
    TOWER_OFF + (height + 1) * std::mem::size_of::<AtomicUsize>()
}

/// Raw pointer to the start of the node at `offset`.
///
/// # Safety
/// `offset` must be a valid node offset previously returned by `alloc_node`.
#[inline]
unsafe fn node_ptr(arena: &Arena, offset: usize) -> *mut u8 {
    arena.get_mut(offset)
}

/// # Safety: see [`node_ptr`].
#[inline]
unsafe fn node_key_len(arena: &Arena, offset: usize) -> usize {
    (node_ptr(arena, offset).add(KEY_LEN_OFF) as *const u32).read() as usize
}

/// # Safety: see [`node_ptr`].
#[inline]
unsafe fn node_value_len(arena: &Arena, offset: usize) -> usize {
    (node_ptr(arena, offset).add(VALUE_LEN_OFF) as *const u32).read() as usize
}

/// # Safety: see [`node_ptr`].
#[inline]
unsafe fn node_height(arena: &Arena, offset: usize) -> usize {
    (node_ptr(arena, offset).add(HEIGHT_OFF) as *const u32).read() as usize
}

/// # Safety: see [`node_ptr`].
#[inline]
unsafe fn node_prev(arena: &Arena, offset: usize) -> &AtomicUsize {
    &*(node_ptr(arena, offset).add(PREV_OFF) as *const AtomicUsize)
}

/// # Safety: see [`node_ptr`]; `level` must be `<= node_height`.
#[inline]
unsafe fn node_tower(arena: &Arena, offset: usize, level: usize) -> &AtomicUsize {
    &*(node_ptr(arena, offset).add(TOWER_OFF + level * std::mem::size_of::<AtomicUsize>())
        as *const AtomicUsize)
}

/// Load the forward offset at `level`. # Safety: see [`node_tower`].
#[inline]
unsafe fn node_next_offset(arena: &Arena, offset: usize, level: usize) -> usize {
    node_tower(arena, offset, level).load(AtomicOrdering::SeqCst)
}

/// Store the forward offset at `level`. # Safety: see [`node_tower`].
#[inline]
unsafe fn node_set_next(arena: &Arena, offset: usize, level: usize, next: usize) {
    node_tower(arena, offset, level).store(next, AtomicOrdering::SeqCst);
}

/// The inline key bytes. # Safety: see [`node_ptr`].
#[inline]
unsafe fn node_key(arena: &Arena, offset: usize) -> &[u8] {
    let height = node_height(arena, offset);
    let key_len = node_key_len(arena, offset);
    let data = node_ptr(arena, offset).add(kv_data_off(height));
    std::slice::from_raw_parts(data, key_len)
}

/// The inline value bytes. # Safety: see [`node_ptr`].
#[inline]
unsafe fn node_value(arena: &Arena, offset: usize) -> &[u8] {
    let height = node_height(arena, offset);
    let key_len = node_key_len(arena, offset);
    let value_len = node_value_len(arena, offset);
    let data = node_ptr(arena, offset).add(kv_data_off(height) + key_len);
    std::slice::from_raw_parts(data, value_len)
}

/// Allocate a node in the arena and copy `key`/`value` inline.
///
/// Only `height + 1` tower slots are reserved (variable-size allocation).
/// Returns the node's arena offset. All fields that are ever read are fully
/// initialized here, before the node is linked into the list.
fn alloc_node(arena: &Arena, key: &[u8], value: &[u8], height: usize) -> usize {
    let size = kv_data_off(height) + key.len() + value.len();
    let offset = arena.alloc(size);
    unsafe {
        let base = node_ptr(arena, offset);
        (base.add(KEY_LEN_OFF) as *mut u32).write(key.len() as u32);
        (base.add(VALUE_LEN_OFF) as *mut u32).write(value.len() as u32);
        (base.add(HEIGHT_OFF) as *mut u32).write(height as u32);
        (base.add(PREV_OFF) as *mut AtomicUsize).write(AtomicUsize::new(0));
        for level in 0..=height {
            (base.add(TOWER_OFF + level * std::mem::size_of::<AtomicUsize>()) as *mut AtomicUsize)
                .write(AtomicUsize::new(0));
        }
        let kv = base.add(kv_data_off(height));
        std::ptr::copy_nonoverlapping(key.as_ptr(), kv, key.len());
        std::ptr::copy_nonoverlapping(value.as_ptr(), kv.add(key.len()), value.len());
    }
    offset
}

/// Inner skip list state (shared between Skiplist handles)
struct SkiplistInner {
    /// Current max height of the skiplist
    height: AtomicUsize,
    /// Offset to the head node
    head: usize,
    /// The arena backing all nodes.
    arena: Arc<Arena>,
}

/// A SkipList with arena allocation and pluggable KeyComparator.
///
/// # Concurrency
///
/// Supports concurrent reads with a single writer; multiple concurrent writers
/// are supported via CAS when `allow_concurrent_write = true`. Nodes are
/// immutable once linked, so a reader can never observe a partially written or
/// overwritten node.
///
/// # Type Parameters
///
/// - `CMP`: KeyComparator implementation for ordering keys
#[derive(Clone)]
pub struct SkipList<CMP> {
    inner: Arc<SkiplistInner>,
    /// Key comparator for ordering keys
    cmp: CMP,
    /// Whether to allow concurrent writes
    allow_concurrent_write: bool,
}

impl<CMP: KeyComparator> SkipList<CMP> {
    /// Create a new skiplist with the given comparator and arena capacity.
    pub fn with_capacity(cmp: CMP, capacity: usize, allow_concurrent_write: bool) -> Self {
        let arena = Arena::with_capacity(capacity);

        // Allocate the head sentinel node (max height, empty key/value).
        let head = alloc_node(&arena, &[], &[], MAX_HEIGHT - 1);

        let inner = SkiplistInner {
            height: AtomicUsize::new(0),
            head,
            arena: Arc::new(arena),
        };

        Self {
            inner: Arc::new(inner),
            cmp,
            allow_concurrent_write,
        }
    }

    /// Get current height.
    #[inline]
    fn list_height(&self) -> usize {
        self.inner.height.load(AtomicOrdering::SeqCst)
    }

    /// Generate a random height with geometric distribution.
    ///
    /// With probability 2/3, returns 0.
    /// With probability 2/9, returns 1.
    /// Etc.
    fn random_height(&self) -> usize {
        let mut rng = rand::thread_rng();
        for h in 0..(MAX_HEIGHT - 1) {
            if !rng.gen_ratio(HEIGHT_INCREASE, u32::MAX) {
                return h;
            }
        }
        MAX_HEIGHT - 1
    }

    /// Get current offset of head.
    #[inline]
    fn head_offset(&self) -> usize {
        self.inner.head
    }

    /// Insert a key-value pair.
    ///
    /// Returns `None` on success.
    /// Returns `Some((key, value))` if the key already exists with a different
    /// value. Nodes are immutable: an existing value is never overwritten in
    /// place (callers version keys via `InternalKey` for MVCC updates).
    pub fn put(&self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Option<(Bytes, Bytes)> {
        let (key, value) = (key.into(), value.into());

        let height = self.random_height();
        let list_height = self.list_height();
        let arena: &Arena = &self.inner.arena;

        // Predecessor / successor offsets at each level.
        // Size MAX_HEIGHT + 1 to match AgateDB: we read prev[i+1] at level i.
        let mut prev = [0usize; MAX_HEIGHT + 1];
        let mut next = [0usize; MAX_HEIGHT + 1];

        for item in prev.iter_mut().take(MAX_HEIGHT + 1).skip(list_height + 1) {
            *item = self.head_offset();
        }

        // Search from the top level down.
        for i in (0..=list_height).rev() {
            let (p, n) = unsafe { self.find_splice_for_level(key.as_ref(), prev[i + 1], i, arena) };
            prev[i] = p;
            next[i] = n;

            // Key already exists at this level.
            if p == n && p != 0 {
                let existing = unsafe { node_value(arena, p) };
                if existing != value.as_ref() {
                    return Some((key, value));
                }
                return None; // Same value, no-op.
            }
        }

        // Allocate the new node (copies key/value inline).
        let node_offset = alloc_node(arena, key.as_ref(), value.as_ref(), height);

        // Raise the list height if needed.
        if height > list_height {
            if self.allow_concurrent_write {
                let mut current = list_height;
                while current < height {
                    match self.inner.height.compare_exchange_weak(
                        current,
                        height,
                        AtomicOrdering::SeqCst,
                        AtomicOrdering::SeqCst,
                    ) {
                        Ok(_) => break,
                        Err(h) => current = h,
                    }
                }
            } else {
                self.inner.height.store(height, AtomicOrdering::SeqCst);
            }
        }

        // Splice the new node in at each level.
        for i in 0..=height {
            if self.allow_concurrent_write {
                loop {
                    if prev[i] == 0 {
                        prev[i] = self.head_offset();
                        let (p, n) =
                            unsafe { self.find_splice_for_level(key.as_ref(), prev[i], i, arena) };
                        prev[i] = p;
                        next[i] = n;
                    }

                    let next_offset = next[i];

                    // Set new node's next first (store-only, no CAS needed).
                    unsafe { node_set_next(arena, node_offset, i, next_offset) };

                    // CAS to link prev[i] -> new_node.
                    let linked = unsafe {
                        node_tower(arena, prev[i], i)
                            .compare_exchange(
                                next_offset,
                                node_offset,
                                AtomicOrdering::SeqCst,
                                AtomicOrdering::SeqCst,
                            )
                            .is_ok()
                    };
                    if linked {
                        break;
                    }

                    // CAS failed: re-search for the correct splice position.
                    let (p, n) =
                        unsafe { self.find_splice_for_level(key.as_ref(), prev[i], i, arena) };
                    if p == n {
                        // Duplicate key discovered at level 0.
                        assert_eq!(i, 0);
                        let existing = unsafe { node_value(arena, p) };
                        if existing != value.as_ref() {
                            return Some((key, value));
                        }
                        // Same value: the freshly allocated node is abandoned in
                        // the arena (nodes own no heap data; nothing to reclaim).
                        return None;
                    }
                    prev[i] = p;
                    next[i] = n;
                }
            } else {
                // Single-threaded: direct stores.
                if prev[i] == 0 {
                    prev[i] = self.head_offset();
                }

                let next_offset = next[i];

                // Build the level-0 prev back-pointer for reverse iteration.
                if i == 0 {
                    unsafe {
                        node_prev(arena, node_offset).store(prev[0], AtomicOrdering::Relaxed);
                        if next_offset != 0 {
                            node_prev(arena, next_offset)
                                .store(node_offset, AtomicOrdering::Release);
                        }
                    }
                }

                unsafe {
                    node_set_next(arena, node_offset, i, next_offset);
                    node_set_next(arena, prev[i], i, node_offset);
                }
            }
        }
        // Note: the `prev` back-pointer is built only in the single-threaded
        // branch (level 0). Under concurrent writes it is left unset and reverse
        // iteration falls back to `find_near`, matching AgateDB.

        None
    }

    /// Find the node just before `key` at `level`, starting from `before`.
    /// Returns `(prev_offset, next_offset)`; returns `(n, n)` when an equal key
    /// is found, which lets `put` detect duplicates.
    ///
    /// # Safety
    /// `before` must be a valid node offset (or 0, meaning "start at head").
    unsafe fn find_splice_for_level(
        &self,
        key: &[u8],
        mut before: usize,
        level: usize,
        arena: &Arena,
    ) -> (usize, usize) {
        loop {
            if before == 0 {
                before = self.head_offset();
            }
            let next_offset = node_next_offset(arena, before, level);

            if next_offset == 0 {
                return (before, 0);
            }

            match self.cmp.compare_key(node_key(arena, next_offset), key) {
                Ordering::Less => before = next_offset,
                Ordering::Greater => return (before, next_offset),
                Ordering::Equal => return (next_offset, next_offset),
            }
        }
    }

    /// Get a value by key.
    pub fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.get_with_key(key).map(|(_, v)| v)
    }

    /// Get the key-value pair for an exact key match, or `None`.
    pub fn get_with_key(&self, key: &[u8]) -> Option<(Bytes, Bytes)> {
        self.lower_bound(key).and_then(|(found_key, value)| {
            if found_key.as_ref() == key {
                Some((found_key, value))
            } else {
                None
            }
        })
    }

    /// Get the first key-value pair whose key is greater than or equal to `key`.
    pub fn lower_bound(&self, key: &[u8]) -> Option<(Bytes, Bytes)> {
        let mut current = self.head_offset();
        let mut level = self.list_height();
        let arena: &Arena = &self.inner.arena;

        loop {
            let next_offset = unsafe { node_next_offset(arena, current, level) };

            if next_offset == 0 {
                if level > 0 {
                    level -= 1;
                    continue;
                }
                return None;
            }

            match self
                .cmp
                .compare_key(unsafe { node_key(arena, next_offset) }, key)
            {
                Ordering::Less => current = next_offset,
                Ordering::Greater => {
                    if level > 0 {
                        level -= 1;
                        continue;
                    }
                    return Some(unsafe {
                        (
                            Bytes::copy_from_slice(node_key(arena, next_offset)),
                            Bytes::copy_from_slice(node_value(arena, next_offset)),
                        )
                    });
                }
                Ordering::Equal => {
                    return Some(unsafe {
                        (
                            Bytes::copy_from_slice(node_key(arena, next_offset)),
                            Bytes::copy_from_slice(node_value(arena, next_offset)),
                        )
                    });
                }
            }
        }
    }

    /// Finds the node near to key.
    ///
    /// If `less` is true, finds the rightmost node such that `node.key < key`
    /// (`allow_equal=false`) or `node.key <= key` (`allow_equal=true`).
    /// If `less` is false, finds the leftmost node such that `node.key > key`
    /// (`allow_equal=false`) or `node.key >= key` (`allow_equal=true`).
    ///
    /// Returns the arena offset of the node, or 0 if not found. Offsets (not raw
    /// pointers) are returned so the result can be stored directly as an
    /// `IterRef` cursor, which addresses nodes by offset.
    unsafe fn find_near(&self, key: &[u8], less: bool, allow_equal: bool) -> usize {
        let mut cursor = self.head_offset();
        let mut level = self.list_height();
        let arena: &Arena = &self.inner.arena;

        loop {
            let next_offset = node_next_offset(arena, cursor, level);

            if next_offset == 0 {
                if level > 0 {
                    level -= 1;
                    continue;
                }
                if !less || cursor == self.head_offset() {
                    return 0;
                }
                return cursor;
            }

            let cmp = self.cmp.compare_key(node_key(arena, next_offset), key);

            if cmp == Ordering::Less {
                cursor = next_offset;
                continue;
            }

            if cmp == Ordering::Equal {
                if allow_equal {
                    return next_offset;
                }
                if !less {
                    // Need to go to the node after the equal one.
                    return node_next_offset(arena, next_offset, 0);
                }
                if level > 0 {
                    level -= 1;
                    continue;
                }
                if cursor == self.head_offset() {
                    return 0;
                }
                return cursor;
            }

            // cmp == Greater
            if level > 0 {
                level -= 1;
                continue;
            }
            if !less {
                return next_offset;
            }
            if cursor == self.head_offset() {
                return 0;
            }
            return cursor;
        }
    }

    /// Finds the last node in the skiplist, returning its arena offset (0 if empty).
    unsafe fn find_last(&self) -> usize {
        let mut cursor = self.head_offset();
        let mut level = self.list_height();
        let arena: &Arena = &self.inner.arena;

        loop {
            let next_offset = node_next_offset(arena, cursor, level);

            if next_offset != 0 {
                cursor = next_offset;
                continue;
            }

            if level == 0 {
                if cursor == self.head_offset() {
                    return 0;
                }
                return cursor;
            }
            level -= 1;
        }
    }

    /// Check if the skiplist contains a key.
    pub fn contains(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// Returns the number of elements.
    pub fn len(&self) -> usize {
        let mut count = 0;
        let mut current = self.head_offset();
        let arena: &Arena = &self.inner.arena;

        loop {
            let next = unsafe { node_next_offset(arena, current, 0) };
            if next == 0 {
                break;
            }
            current = next;
            count += 1;
        }

        count
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the total memory used by the skiplist (arena allocated bytes).
    pub fn mem_size(&self) -> usize {
        self.inner.arena.len()
    }

    /// Returns a reference-style iterator over the skiplist.
    pub fn iter_ref(&self) -> IterRef<'_, CMP> {
        IterRef {
            list: self,
            cursor: 0, // 0 means not positioned (needs seek_to_first or similar)
            _cmp: std::marker::PhantomData,
        }
    }
}

impl<CMP: KeyComparator> AsRef<SkipList<CMP>> for SkipList<CMP> {
    fn as_ref(&self) -> &SkipList<CMP> {
        self
    }
}

// Note: no `Drop` for `SkiplistInner`. Nodes own no heap data (key/value are
// inline in the arena), so dropping the `Arena` buffer reclaims everything.

unsafe impl<CMP: KeyComparator> Send for SkipList<CMP> {}
unsafe impl<CMP: KeyComparator> Sync for SkipList<CMP> {}

/// Iterator for traversing a SkipList.
///
/// Provides forward and backward traversal, and O(log n) seek operations.
pub struct IterRef<'a, CMP: KeyComparator> {
    list: &'a SkipList<CMP>,
    /// Current node offset (0 means not valid / not positioned)
    cursor: usize,
    _cmp: std::marker::PhantomData<CMP>,
}

impl<'a, CMP: KeyComparator> IterRef<'a, CMP> {
    /// Returns true if the iterator is positioned at a valid node.
    pub fn valid(&self) -> bool {
        self.cursor != 0
    }

    /// Returns the key at the current position (borrowed from the arena).
    pub fn key(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        Some(unsafe { node_key(&self.list.inner.arena, self.cursor) })
    }

    /// Returns the value at the current position (borrowed from the arena).
    pub fn value(&self) -> Option<&[u8]> {
        if !self.valid() {
            return None;
        }
        Some(unsafe { node_value(&self.list.inner.arena, self.cursor) })
    }

    /// Advances to the next node in the skiplist.
    pub fn next(&mut self) {
        if !self.valid() {
            return;
        }
        self.cursor = unsafe { node_next_offset(&self.list.inner.arena, self.cursor, 0) };
    }

    /// Moves to the previous node in the skiplist.
    pub fn prev(&mut self) {
        if !self.valid() {
            return;
        }
        if self.list.allow_concurrent_write {
            // Concurrent mode does not maintain `prev`; re-derive via find_near.
            if let Some(k) = self.key() {
                self.cursor = unsafe { self.list.find_near(k, true, false) };
            }
        } else {
            // Single-writer mode: follow the level-0 prev back-pointer.
            self.cursor = unsafe {
                node_prev(&self.list.inner.arena, self.cursor).load(AtomicOrdering::SeqCst)
            };
            if self.cursor == self.list.inner.head {
                self.cursor = 0;
            }
        }
    }

    /// Seeks to the first node whose key >= target.
    pub fn seek(&mut self, target: &[u8]) {
        self.cursor = unsafe { self.list.find_near(target, false, true) };
    }

    /// Seeks to the last node whose key <= target.
    pub fn seek_for_prev(&mut self, target: &[u8]) {
        self.cursor = unsafe { self.list.find_near(target, true, true) };
    }

    /// Seeks to the first key in the skiplist.
    pub fn seek_to_first(&mut self) {
        self.cursor = unsafe { node_next_offset(&self.list.inner.arena, self.list.inner.head, 0) };
    }

    /// Seeks to the last key in the skiplist.
    pub fn seek_to_last(&mut self) {
        self.cursor = unsafe { self.list.find_last() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skl() -> SkipList<BytewiseComparator> {
        SkipList::with_capacity(BytewiseComparator::new(), 4 * 1024 * 1024, false)
    }

    #[test]
    fn test_basic_insert() {
        let sl = make_skl();

        sl.put(&b"key1"[..], &b"value1"[..]);
        sl.put(&b"key2"[..], &b"value2"[..]);

        assert_eq!(sl.get(&b"key1"[..]), Some(Bytes::from(b"value1".to_vec())));
        assert_eq!(sl.get(&b"key2"[..]), Some(Bytes::from(b"value2".to_vec())));
        assert_eq!(sl.get(&b"key3"[..]), None);
    }

    #[test]
    fn test_put_existing_key_is_insert_only() {
        // Arrange: a key is already present.
        let sl = make_skl();
        sl.put(&b"key"[..], &b"value1"[..]);

        // Act: put the same key with a different value.
        let conflict = sl.put(&b"key"[..], &b"value2"[..]);

        // Assert: nodes are immutable, so the put reports a conflict and does
        // NOT overwrite. Versioning keys is the caller's responsibility.
        assert_eq!(
            conflict,
            Some((
                Bytes::from(b"key".to_vec()),
                Bytes::from(b"value2".to_vec())
            ))
        );
        assert_eq!(sl.get(&b"key"[..]), Some(Bytes::from(b"value1".to_vec())));
    }

    #[test]
    fn test_contains() {
        let sl = make_skl();

        sl.put(&b"key"[..], &b"value"[..]);
        assert!(sl.contains(&b"key"[..]));
        assert!(!sl.contains(&b"notexist"[..]));
    }

    #[test]
    fn test_empty() {
        let sl = make_skl();
        assert!(sl.is_empty());
        assert_eq!(sl.len(), 0);

        sl.put(&b"key"[..], &b"value"[..]);
        assert!(!sl.is_empty());
        assert_eq!(sl.len(), 1);
    }

    #[test]
    fn test_long_key_value_roundtrip() {
        // Arrange: key and value larger than a cache line, to exercise the
        // inline offset arithmetic beyond the node header and tower.
        let sl = make_skl();
        let key = vec![0xABu8; 200];
        let value = vec![0xCDu8; 4096];

        // Act
        sl.put(Bytes::from(key.clone()), Bytes::from(value.clone()));

        // Assert: round-trips through get and through the iterator.
        assert_eq!(sl.get(&key), Some(Bytes::from(value.clone())));
        let mut it = sl.iter_ref();
        it.seek_to_first();
        assert_eq!(it.key(), Some(key.as_slice()));
        assert_eq!(it.value(), Some(value.as_slice()));
    }
}
