//! Iterator Trait
//!
//! Defines the interface for iterating over LSM tree data structures.

use crate::error::Result;

/// Base iterator trait for LSM tree components.
/// All iterators return entries in sorted key order.
pub trait Iterator {
    /// Seek to a specific key, positioning at the first entry >= key.
    fn seek(&mut self, key: &[u8]);

    /// Seek to the first key.
    fn seek_to_first(&mut self);

    /// Seek to the last key.
    fn seek_to_last(&mut self);

    /// Advance to the next entry.
    fn next(&mut self);

    /// Advance to the previous entry.
    fn prev(&mut self);

    /// Returns true if the iterator is at a valid position.
    fn valid(&self) -> bool;

    /// Returns the current key.
    fn key(&self) -> &[u8];

    /// Returns the current value.
    fn value(&self) -> &[u8];

    /// Returns the current sequence number.
    fn sequence(&self) -> u64;

    /// Returns true if the current entry is a deletion (tombstone).
    fn is_delete(&self) -> bool;

    /// Check the status of the iterator.
    fn status(&mut self) -> Result<()>;
}
