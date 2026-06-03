//! Arena Allocator
//!
//! Pre-allocated memory pool for SkipList nodes. All allocations are O(1)
//! with no syscall overhead. Memory is released when the Arena is dropped.
//!
//! Based on AgateDB's arena implementation.

use std::alloc::{alloc, dealloc, Layout};
use std::mem;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Alignment mask for 8-byte alignment
const ADDR_ALIGN_MASK: usize = 7;

/// A pre-allocated memory pool for SkipList nodes.
///
/// All nodes are allocated from this pool using simple bump-the-pointer
/// allocation. No malloc is called after initialization.
pub struct Arena {
    /// Pointer to the start of the arena
    start: *mut u8,
    /// Current allocation head
    ptr: AtomicPtr<u8>,
    /// Total capacity in bytes
    capacity: usize,
}

impl Arena {
    /// Create a new arena with the specified capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        let layout = Layout::from_size_align(capacity, mem::align_of::<usize>()).unwrap();
        let start = unsafe { alloc(layout) };
        if start.is_null() {
            panic!("Arena allocation failed");
        }
        Self {
            // Offset 0 is invalid for offset/get_mut, so start at 8
            start,
            ptr: AtomicPtr::new(std::ptr::null_mut()),
            capacity,
        }
    }

    /// Allocate `size` bytes from the arena.
    ///
    /// Returns the offset from the start of the arena.
    /// Panics if out of memory.
    pub fn alloc(&self, size: usize) -> usize {
        // Align size to 8 bytes
        let size = (size + ADDR_ALIGN_MASK) & !ADDR_ALIGN_MASK;

        loop {
            let current_ptr = self.ptr.load(Ordering::SeqCst);
            let offset = if current_ptr.is_null() {
                // First allocation - leave room for offset 0 (invalid)
                8
            } else {
                unsafe { current_ptr.offset_from(self.start) as usize }
            };

            // Check if we have enough space
            if offset + size > self.capacity {
                panic!(
                    "Arena out of memory: requested {} bytes, {} remaining",
                    size,
                    self.capacity - offset
                );
            }

            let new_ptr = if current_ptr.is_null() {
                unsafe { self.start.add(offset + size) }
            } else {
                unsafe { current_ptr.add(size) }
            };

            // Try to update ptr atomically
            match self.ptr.compare_exchange(
                current_ptr,
                new_ptr,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return offset,
                Err(_) => continue, // Another thread modified ptr, retry
            }
        }
    }

    /// Get the current offset (bytes allocated so far).
    #[inline]
    pub fn offset(&self) -> usize {
        let ptr = self.ptr.load(Ordering::SeqCst);
        if ptr.is_null() {
            0
        } else {
            unsafe { ptr.offset_from(self.start) as usize }
        }
    }

    /// Get a mutable pointer to a given offset.
    ///
    /// # Safety
    /// The offset must be valid and within the arena.
    #[inline]
    pub unsafe fn get_mut(&self, offset: usize) -> *mut u8 {
        if offset == 0 {
            std::ptr::null_mut()
        } else {
            self.start.add(offset)
        }
    }

    /// Get the total bytes allocated.
    #[inline]
    pub fn len(&self) -> usize {
        self.offset()
    }

    /// Get the remaining bytes available.
    #[inline]
    #[cfg(test)]
    pub fn remaining(&self) -> usize {
        self.capacity - self.offset()
    }

    /// Check if arena is empty.
    #[inline]
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.offset() == 0
    }
}

// Safety: Arena uses atomic operations for thread-safe allocation.
unsafe impl Send for Arena {}
unsafe impl Sync for Arena {}

impl Drop for Arena {
    fn drop(&mut self) {
        unsafe {
            let layout = Layout::from_size_align(self.capacity, mem::align_of::<usize>()).unwrap();
            dealloc(self.start, layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_alloc() {
        let arena = Arena::with_capacity(1024);

        // First allocation starts at offset 8 (offset 0 is reserved as invalid)
        let off1 = arena.alloc(100); // 100 aligned to 104
        assert_eq!(off1, 8);
        // len() returns the next free offset, not actual bytes used
        assert_eq!(arena.len(), 112); // 8 + 104 = 112
        assert_eq!(arena.remaining(), 1024 - 112);

        let off2 = arena.alloc(50); // 50 aligned to 56
        assert_eq!(off2, 112);
        assert_eq!(arena.len(), 168); // 112 + 56 = 168

        // Check we can write and read via offset
        unsafe {
            let ptr = arena.get_mut(off1);
            std::ptr::write(ptr as *mut u64, 42);
            assert_eq!(std::ptr::read(ptr as *const u64), 42);
        }
    }

    #[test]
    fn test_arena_overflow() {
        // Arena with 256 bytes - after first 100-byte alloc (aligned to 104),
        // offset=8, ptr moves to 112, remaining=144
        // Second 100-byte alloc starts at 112, uses 104, ptr moves to 216, remaining=40
        let arena = Arena::with_capacity(256);

        let off1 = arena.alloc(100);
        assert_eq!(off1, 8);
        assert_eq!(arena.len(), 112); // 8 + 104
        assert_eq!(arena.remaining(), 256 - 112);

        // Second 100-byte alloc uses 104 bytes starting at offset 112
        let off2 = arena.alloc(100);
        assert_eq!(off2, 112);
        assert_eq!(arena.len(), 216); // 112 + 104

        // 40 bytes remaining - 40 is not enough for a 48-byte (aligned 1) allocation
        assert_eq!(arena.remaining(), 40);

        // Next allocation of 41 bytes (aligned to 48) should panic since only 40 remaining
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| arena.alloc(41)));
        assert!(result.is_err());
    }

    #[test]
    fn test_arena_empty() {
        let arena = Arena::with_capacity(1024);
        assert!(arena.is_empty());
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.remaining(), 1024);
    }

    #[test]
    fn test_arena_get_mut() {
        let arena = Arena::with_capacity(1024);
        let off = arena.alloc(16);

        unsafe {
            let ptr = arena.get_mut(off);
            assert!(!ptr.is_null());

            // Offset 0 returns null
            let null_ptr = arena.get_mut(0);
            assert!(null_ptr.is_null());
        }
    }
}
