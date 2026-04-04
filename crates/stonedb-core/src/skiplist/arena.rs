//! Arena Allocator
//!
//! Pre-allocated memory pool for SkipList nodes. All allocations are O(1)
//! with no syscall overhead. Memory is released when the Arena is dropped.
//!
//! Based on AgateDB's arena implementation.

use std::alloc::{alloc, dealloc, Layout};
use std::mem;

/// A pre-allocated memory pool for SkipList nodes.
///
/// All nodes are allocated from this pool using simple bump-the-pointer
/// allocation. No malloc is called after initialization.
pub struct Arena {
    /// Pointer to the start of the arena
    start: *mut u8,
    /// Pointer to the current allocation head
    ptr: *mut u8,
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
            start,
            ptr: start,
            capacity,
        }
    }

    /// Allocate `size` bytes from the arena.
    ///
    /// Returns the offset from the start of the arena.
    /// Panics if out of memory.
    pub fn alloc(&mut self, size: usize) -> usize {
        let offset = self.offset();
        let new_ptr = self.ptr.wrapping_add(size);

        // Check for overflow or out of memory
        if new_ptr > self.start.wrapping_add(self.capacity) || new_ptr < self.ptr {
            panic!(
                "Arena out of memory: requested {} bytes, {} remaining",
                size,
                self.remaining()
            );
        }

        self.ptr = new_ptr;
        offset
    }

    /// Get the current offset (bytes allocated so far).
    #[inline]
    pub fn offset(&self) -> usize {
        unsafe { self.ptr.offset_from(self.start) as usize }
    }

    /// Get a mutable pointer to a given offset.
    ///
    /// # Safety
    /// The offset must be valid and within the arena.
    #[inline]
    pub unsafe fn get_mut(&self, offset: usize) -> *mut u8 {
        self.start.add(offset)
    }

    /// Get offset from a pointer within the arena.
    #[inline]
    #[allow(dead_code)]
    pub unsafe fn offset_from(&self, ptr: *const u8) -> usize {
        ptr.offset_from(self.start) as usize
    }

    /// Get the total bytes allocated.
    #[inline]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.offset()
    }

    /// Get the remaining bytes available.
    #[inline]
    #[allow(dead_code)]
    pub fn remaining(&self) -> usize {
        self.capacity - self.offset()
    }

    /// Check if arena is empty.
    #[inline]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.offset() == 0
    }
}

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
        let mut arena = Arena::with_capacity(1024);

        let off1 = arena.alloc(100);
        assert_eq!(off1, 0);
        assert_eq!(arena.len(), 100);

        let off2 = arena.alloc(50);
        assert_eq!(off2, 100);
        assert_eq!(arena.len(), 150);

        // Check we can write and read via offset
        unsafe {
            let ptr = arena.get_mut(off1);
            std::ptr::write(ptr as *mut u64, 42);
            assert_eq!(std::ptr::read(ptr as *const u64), 42);
        }
    }

    #[test]
    fn test_arena_overflow() {
        let mut arena = Arena::with_capacity(100);

        arena.alloc(100);
        assert_eq!(arena.remaining(), 0);

        // Next allocation should panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| arena.alloc(1)));
        assert!(result.is_err());
    }
}
