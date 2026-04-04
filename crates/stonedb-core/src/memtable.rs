//! MemTable - In-Memory Sorted Key-Value Store
//!
//! MemTable is the in-memory component of the LSM tree. All writes go here first,
//! and when it becomes full, it is flushed to SSTables on disk.

use crate::entry::{Entry, ValueType};
use crate::error::Result;
use crate::iterator::Iterator;
use crate::key::InternalKey;
use crate::skiplist::{Node, SkipList};
use std::sync::atomic::{AtomicU64, Ordering};

/// The in-memory table storing key-value pairs.
pub struct MemTable {
    /// The underlying skiplist storing InternalKeys to values
    list: SkipList<InternalKey, Vec<u8>>,
    /// Next sequence number to assign
    next_sequence: AtomicU64,
    /// Approximate memory usage
    approximate_size: AtomicU64,
    /// ID for identification
    id: usize,
}

impl MemTable {
    /// Create a new MemTable with the given ID.
    pub fn new(id: usize) -> Self {
        Self {
            list: SkipList::new(),
            next_sequence: AtomicU64::new(1),
            approximate_size: AtomicU64::new(0),
            id,
        }
    }

    /// Returns the approximate memory usage of this MemTable.
    pub fn approximate_size(&self) -> u64 {
        self.approximate_size.load(Ordering::Relaxed)
    }

    /// Returns the next sequence number that will be assigned.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence.load(Ordering::Acquire)
    }

    /// Returns the ID of this MemTable.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Returns the number of entries in this MemTable.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// Returns true if the MemTable is empty.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Insert a value, returning the sequence number.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<u64> {
        let seq = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        if seq == u64::MAX {
            return Err(crate::error::Error::SequenceOverflow);
        }

        let internal_key = InternalKey::new_put(key, seq);
        self.list.insert(internal_key, value.to_vec());

        let size = key.len() as u64 + value.len() as u64 + 8;
        self.approximate_size.fetch_add(size, Ordering::Relaxed);

        Ok(seq)
    }

    /// Insert a delete (tombstone), returning the sequence number.
    pub fn delete(&mut self, key: &[u8]) -> Result<u64> {
        let seq = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        if seq == u64::MAX {
            return Err(crate::error::Error::SequenceOverflow);
        }

        let internal_key = InternalKey::new_delete(key, seq);
        self.list.insert(internal_key, Vec::new());

        let size = key.len() as u64 + 8;
        self.approximate_size.fetch_add(size, Ordering::Relaxed);

        Ok(seq)
    }

    /// Get a value by key, returning the newest non-deleted entry.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let search_key = InternalKey::new_max(key);

        if let Some((found_key, value)) = self.list.lower_bound(&search_key) {
            if found_key.user_key() == key {
                if found_key.value_type() == ValueType::Value {
                    return Ok(Some(value));
                } else {
                    return Ok(None);
                }
            }
        }

        Ok(None)
    }

    /// Check if a key exists (returns true even for tombstones).
    pub fn contains(&self, key: &[u8]) -> bool {
        self.get_entry(key).map(|e| e.is_some()).unwrap_or(false)
    }

    /// Get an entry by key (including tombstones).
    pub fn get_entry(&self, key: &[u8]) -> Result<Option<Entry>> {
        let search_key = InternalKey::new_max(key);

        if let Some((found_key, value)) = self.list.lower_bound(&search_key) {
            if found_key.user_key() == key {
                return Ok(Some(Entry {
                    key: found_key.user_key().to_vec(),
                    value,
                    sequence: found_key.sequence(),
                    value_type: found_key.value_type(),
                }));
            }
        }

        Ok(None)
    }

    /// Create an iterator over all entries in the MemTable.
    pub fn iter(&self) -> MemTableIterator<'_> {
        MemTableIterator::new(self)
    }
}

impl Default for MemTable {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Iterator over a MemTable.
pub struct MemTableIterator<'a> {
    list_iter: crate::skiplist::SkipListIterator<'a, InternalKey, Vec<u8>>,
    current_key: Vec<u8>,
    current_value: Vec<u8>,
    current_seq: u64,
    current_is_delete: bool,
}

impl<'a> MemTableIterator<'a> {
    fn new(memtable: &'a MemTable) -> Self {
        Self {
            list_iter: memtable.list.iter(),
            current_key: Vec::new(),
            current_value: Vec::new(),
            current_seq: 0,
            current_is_delete: false,
        }
    }

    fn update_from_node(&mut self, node: &Node<InternalKey, Vec<u8>>) {
        self.current_key = node.key.user_key().to_vec();
        self.current_value = node.value.clone();
        self.current_seq = node.key.sequence();
        self.current_is_delete = node.key.value_type() == ValueType::Delete;
    }
}

impl<'a> Iterator for MemTableIterator<'a> {
    fn seek(&mut self, key: &[u8]) {
        // Create a search key with MAX_SEQUENCE to find newest entry for this user key
        let search_key = InternalKey::new_max(key);
        self.list_iter.seek(&search_key);
        if let Some(node) = self.list_iter.current() {
            self.update_from_node(node);
        } else {
            self.current_key.clear();
            self.current_value.clear();
            self.current_seq = 0;
            self.current_is_delete = false;
        }
    }

    fn seek_to_first(&mut self) {
        self.list_iter.seek_to_first();
        if let Some(node) = self.list_iter.current() {
            self.update_from_node(node);
        }
    }

    fn seek_to_last(&mut self) {
        // Not implemented
    }

    fn next(&mut self) {
        if let Some(node) = self.list_iter.next() {
            self.update_from_node(node);
        } else {
            self.current_key.clear();
            self.current_value.clear();
            self.current_seq = 0;
            self.current_is_delete = false;
        }
    }

    fn prev(&mut self) {
        // Not implemented
    }

    fn valid(&self) -> bool {
        !self.current_key.is_empty()
    }

    fn key(&self) -> &[u8] {
        &self.current_key
    }

    fn value(&self) -> &[u8] {
        &self.current_value
    }

    fn sequence(&self) -> u64 {
        self.current_seq
    }

    fn is_delete(&self) -> bool {
        self.current_is_delete
    }

    fn status(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let mut memtable = MemTable::new(0);

        let seq1 = memtable.put(b"key1", b"value1").unwrap();
        let seq2 = memtable.put(b"key2", b"value2").unwrap();
        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);

        assert_eq!(memtable.get(b"key1").unwrap(), Some(b"value1".to_vec()));
        assert_eq!(memtable.get(b"key2").unwrap(), Some(b"value2".to_vec()));
        assert_eq!(memtable.get(b"key3").unwrap(), None);
    }

    #[test]
    fn test_update() {
        let mut memtable = MemTable::new(0);

        let seq1 = memtable.put(b"key", b"value1").unwrap();
        assert_eq!(seq1, 1);

        let seq2 = memtable.put(b"key", b"value2").unwrap();
        assert_eq!(seq2, 2);

        assert_eq!(memtable.get(b"key").unwrap(), Some(b"value2".to_vec()));
    }

    #[test]
    fn test_delete() {
        let mut memtable = MemTable::new(0);

        memtable.put(b"key", b"value").unwrap();
        memtable.delete(b"key").unwrap();

        assert_eq!(memtable.get(b"key").unwrap(), None);
    }

    #[test]
    fn test_sequence_numbers() {
        let mut memtable = MemTable::new(0);

        assert_eq!(memtable.next_sequence(), 1);

        memtable.put(b"a", b"1").unwrap();
        assert_eq!(memtable.next_sequence(), 2);

        memtable.put(b"b", b"2").unwrap();
        assert_eq!(memtable.next_sequence(), 3);

        let entry = memtable.get_entry(b"a").unwrap().unwrap();
        assert_eq!(entry.sequence, 1);
    }

    #[test]
    fn test_empty() {
        let mut memtable = MemTable::new(0);
        assert!(memtable.is_empty());
        assert_eq!(memtable.len(), 0);

        memtable.put(b"key", b"value").unwrap();
        assert!(!memtable.is_empty());
        assert_eq!(memtable.len(), 1);
    }
}
