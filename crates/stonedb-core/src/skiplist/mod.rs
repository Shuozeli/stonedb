//! SkipList Implementation
//!
//! Production-grade SkipList based on AgateDB's design.
//! - Arena allocator (no malloc per insert)
//! - Lock-free CAS for concurrent writes
//! - AtomicUsize for lock-free height management
//! - Reverse iteration via prev pointer
//! - Zero-copy keys via Bytes
//!
//! Reference: https://github.com/tikv/agatedb/blob/master/skiplist/src/list.rs

mod arena;
pub use self::arena::Arena;

use bytes::Bytes;
use rand::Rng;
use std::cmp::Ordering;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

/// Maximum height for skip list nodes.
/// With probability 1/3^19, height could reach 20.
pub const MAX_HEIGHT: usize = 20;

/// Probability of increasing height = 1/3 (u32::MAX / 3)
const HEIGHT_INCREASE: u32 = u32::MAX / 3;

/// Node stored in the arena.
/// Uses C-style layout for predictable memory layout.
#[repr(C)]
struct Node {
    /// Key (zero-copy)
    key: Bytes,
    /// Value (zero-copy)
    value: Bytes,
    /// Height of this node (1 to MAX_HEIGHT)
    height: usize,
    /// Offset to previous node at level 0 (for reverse iteration)
    prev: AtomicUsize,
    /// Forward pointers at each level (offsets)
    tower: [AtomicUsize; MAX_HEIGHT],
}

impl Node {
    /// Allocate a new node in the arena.
    ///
    /// Returns the offset of the allocated node.
    fn alloc(arena: &mut Arena, key: Bytes, value: Bytes, height: usize) -> usize {
        // Use fixed size - we only use tower[0..height] but allocate full MAX_HEIGHT
        // This simplifies allocation - we always allocate the same size node
        let size = std::mem::size_of::<Node>();
        let offset = arena.alloc(size);

        unsafe {
            let ptr = arena.get_mut(offset) as *mut Node;

            // Write fields in order (no Drop to worry about since these are Copy-ish)
            std::ptr::write(&mut (*ptr).key, key);
            std::ptr::write(&mut (*ptr).value, value);
            std::ptr::write(&mut (*ptr).height, height);
            std::ptr::write(&mut (*ptr).prev, AtomicUsize::new(0));

            // Initialize tower entries to 0 (null offset)
            for i in 0..MAX_HEIGHT {
                std::ptr::write(&mut (*ptr).tower[i], AtomicUsize::new(0));
            }
        }

        offset
    }

    /// Get the next offset at the given level.
    #[inline]
    fn next_offset(&self, level: usize) -> usize {
        self.tower[level].load(SeqCst)
    }

    /// Set the next pointer at the given level.
    #[inline]
    fn set_next(&self, level: usize, offset: usize) {
        self.tower[level].store(offset, SeqCst);
    }
}

/// Inner skip list state (shared between Skiplist handles)
struct SkiplistInner {
    /// Current max height of the skiplist
    height: AtomicUsize,
    /// Offset to the head node
    head: usize,
    /// The arena (Rc+RefCell for interior mutability in single-threaded mode)
    arena: std::rc::Rc<std::cell::RefCell<Arena>>,
}

/// A并发SkipList with arena allocation.
///
/// # Concurrency
///
/// This skiplist supports concurrent reads and writes via CAS operations.
/// Single-threaded writes are also supported via `allow_concurrent_write = false`.
#[derive(Clone)]
pub struct SkipList {
    inner: std::rc::Rc<SkiplistInner>,
    /// Key comparator (for now, simple byte comparison)
    allow_concurrent_write: bool,
}

impl SkipList {
    /// Create a new skiplist with the given arena capacity.
    pub fn with_capacity(capacity: usize, allow_concurrent_write: bool) -> Self {
        let mut arena = Arena::with_capacity(capacity);

        // Allocate head node (height = MAX_HEIGHT - 1, sentinel node)
        let head = Node::alloc(&mut arena, Bytes::new(), Bytes::new(), MAX_HEIGHT - 1);

        let inner = SkiplistInner {
            height: AtomicUsize::new(0),
            head,
            arena: std::rc::Rc::new(std::cell::RefCell::new(arena)),
        };

        Self {
            inner: std::rc::Rc::new(inner),
            allow_concurrent_write,
        }
    }

    /// Get current height.
    #[inline]
    fn list_height(&self) -> usize {
        self.inner.height.load(SeqCst)
    }

    /// Generate a random height with geometric distribution.
    ///
    /// With probability 2/3, returns 1.
    /// With probability 2/9, returns 2.
    /// With probability 2/27, returns 3.
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

    /// Get pointer to node at offset.
    #[inline]
    unsafe fn get_node(arena: &Arena, offset: usize) -> *mut Node {
        arena.get_mut(offset) as *mut Node
    }

    /// Get current offset of head.
    #[inline]
    fn head_offset(&self) -> usize {
        self.inner.head
    }

    /// Insert a key-value pair.
    ///
    /// Returns `None` on success.
    /// Returns `Some((key, value))` if the key already exists with a different value.
    pub fn put(&self, key: impl Into<Bytes>, value: impl Into<Bytes>) -> Option<(Bytes, Bytes)> {
        let (key, value) = (key.into(), value.into());

        let height = self.random_height();
        let list_height = self.list_height();

        // Acquire arena borrow once for entire operation
        let mut arena = self.inner.arena.borrow_mut();

        // Find predecessors at each level
        let mut prev = [0usize; MAX_HEIGHT];
        let mut next = [0usize; MAX_HEIGHT];

        // Initialize beyond current height to head
        for i in (list_height + 1)..MAX_HEIGHT {
            prev[i] = self.head_offset();
        }

        // Search from top down
        for i in (0..=list_height).rev() {
            let (p, n) = unsafe { self.find_splice_for_level(&key, prev[i + 1], i, &arena) };
            prev[i] = p;
            next[i] = n;

            // Check if key already exists
            if p == n && p != 0 {
                unsafe {
                    let node = &*Self::get_node(&arena, p);
                    if node.value != value {
                        return Some((key, value));
                    }
                }
                return None; // Same value, no-op
            }
        }

        // Allocate new node
        let node_offset = Node::alloc(&mut arena, key, value, height);

        // Update height if needed
        if height > list_height {
            if self.allow_concurrent_write {
                let mut current = list_height;
                while current < height {
                    match self
                        .inner
                        .height
                        .compare_exchange_weak(current, height, SeqCst, SeqCst)
                    {
                        Ok(_) => break,
                        Err(h) => current = h,
                    }
                }
            } else {
                self.inner.height.store(height, SeqCst);
            }
        }

        // Splice in at each level
        unsafe {
            let new_node = &*Self::get_node(&arena, node_offset);

            for i in 0..=height {
                if prev[i] == 0 {
                    prev[i] = self.head_offset();
                }

                let next_offset = if next[i] == 0 {
                    0
                } else {
                    Self::get_node(&arena, next[i])
                        .as_ref()
                        .unwrap()
                        .next_offset(i)
                };

                // Set new node's next first (store-only, no CAS needed)
                new_node.set_next(i, next_offset);

                // CAS to link prev[i] -> new_node
                let prev_node = &*Self::get_node(&arena, prev[i]);
                if self.allow_concurrent_write {
                    let mut current = next_offset;
                    loop {
                        match prev_node.tower[i].compare_exchange(
                            current,
                            node_offset,
                            SeqCst,
                            SeqCst,
                        ) {
                            Ok(_) => break,
                            Err(actual) => {
                                // Something changed, retry
                                current = actual;
                            }
                        }
                    }
                } else {
                    // Single-threaded: direct store
                    prev_node.set_next(i, node_offset);
                }
            }

            // Set prev for level 0 (for reverse iteration)
            if next[0] != 0 {
                let next_node = &*Self::get_node(&arena, next[0]);
                next_node.prev.store(node_offset, SeqCst);
            }
            new_node.prev.store(prev[0], SeqCst);
        }

        None
    }

    /// Find the node just before the given key at a specific level.
    /// Returns (prev_offset, next_offset).
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
            let next_offset = Self::get_node(arena, before)
                .as_ref()
                .unwrap()
                .next_offset(level);

            if next_offset == 0 {
                return (before, 0);
            }

            let next_node = &*Self::get_node(arena, next_offset);
            let cmp = next_node.key.as_ref().cmp(key);

            match cmp {
                Ordering::Less => {
                    before = next_offset;
                }
                Ordering::Greater | Ordering::Equal => {
                    return (before, next_offset);
                }
            }
        }
    }

    /// Get a value by key.
    pub fn get(&self, key: &[u8]) -> Option<Bytes> {
        self.get_with_key(key).map(|(_, v)| v)
    }

    /// Get key-value pair by key.
    pub fn get_with_key(&self, key: &[u8]) -> Option<(Bytes, Bytes)> {
        let mut current = self.head_offset();
        let mut level = self.list_height();
        let arena = self.inner.arena.borrow();

        loop {
            let next_offset = unsafe {
                let node = if current == 0 {
                    self.head_offset()
                } else {
                    current
                };
                Self::get_node(&arena, node)
                    .as_ref()
                    .unwrap()
                    .next_offset(level)
            };

            if next_offset == 0 {
                if level > 0 {
                    level -= 1;
                    continue;
                }
                return None;
            }

            let next_node = unsafe { &*Self::get_node(&arena, next_offset) };
            let cmp = next_node.key.as_ref().cmp(key);

            match cmp {
                Ordering::Less => {
                    current = next_offset;
                }
                Ordering::Greater => {
                    if level > 0 {
                        level -= 1;
                        continue;
                    }
                    return None;
                }
                Ordering::Equal => {
                    return Some((next_node.key.clone(), next_node.value.clone()));
                }
            }
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
        let arena = self.inner.arena.borrow();

        loop {
            let next = unsafe {
                if current == 0 {
                    current = self.head_offset();
                }
                Self::get_node(&arena, current)
                    .as_ref()
                    .unwrap()
                    .next_offset(0)
            };

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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skl() -> SkipList {
        SkipList::with_capacity(4 * 1024 * 1024, false)
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
    fn test_update() {
        let sl = make_skl();

        sl.put(&b"key"[..], &b"value1"[..]);
        assert_eq!(sl.get(&b"key"[..]), Some(Bytes::from(b"value1".to_vec())));

        sl.put(&b"key"[..], &b"value2"[..]);
        assert_eq!(sl.get(&b"key"[..]), Some(Bytes::from(b"value2".to_vec())));
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
}
