//! MemTable - In-Memory Sorted Key-Value Store
//!
//! MemTable is the in-memory component of the LSM tree. All writes go here first,
//! and when it becomes full, it is flushed to SSTables on disk.

use crate::entry::{Entry, ValueType};
use crate::error::Result;
use crate::iterator::Iterator;
use crate::key::{InternalKey, InternalKeyComparator, MAX_SEQUENCE};
use crate::skiplist::SkipList;
use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};

/// The in-memory table storing key-value pairs.
pub struct MemTable {
    /// The underlying skiplist storing InternalKeys to values
    list: SkipList<InternalKeyComparator>,
    /// Next sequence number to assign
    next_sequence: AtomicU64,
    /// Approximate memory usage
    approximate_size: AtomicU64,
    /// ID for identification
    id: usize,
}

impl MemTable {
    /// Create a new MemTable with the given ID and capacity.
    pub fn with_capacity(id: usize, capacity: usize) -> Self {
        Self {
            list: SkipList::with_capacity(InternalKeyComparator::new(), capacity, false),
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
        if seq > MAX_SEQUENCE {
            return Err(crate::error::Error::SequenceOverflow);
        }

        // Every write is a distinct, immutable node keyed by its InternalKey
        // (user_key | sequence | type). The skiplist never mutates an existing
        // node; newer versions simply sort ahead of older ones.
        let ikey = InternalKey::new_put(key, seq);
        self.list
            .put(Bytes::from(ikey.encoded), Bytes::copy_from_slice(value));

        let size = key.len() as u64 + value.len() as u64 + 8;
        self.approximate_size.fetch_add(size, Ordering::Relaxed);

        Ok(seq)
    }

    /// Insert a delete (tombstone), returning the sequence number.
    pub fn delete(&mut self, key: &[u8]) -> Result<u64> {
        let seq = self.next_sequence.fetch_add(1, Ordering::AcqRel);
        if seq > MAX_SEQUENCE {
            return Err(crate::error::Error::SequenceOverflow);
        }

        // A delete is a tombstone: an InternalKey with type=Delete and an empty
        // value. It sorts ahead of older versions of the same user key.
        let ikey = InternalKey::new_delete(key, seq);
        self.list.put(Bytes::from(ikey.encoded), Bytes::new());

        let size = key.len() as u64 + 8;
        self.approximate_size.fetch_add(size, Ordering::Relaxed);

        Ok(seq)
    }

    /// Get a value by key, returning the newest version (None if the key is
    /// absent or its newest version is a tombstone).
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.lookup(key) {
            Some(entry) if entry.value_type == ValueType::Value => Ok(Some(entry.value)),
            _ => Ok(None),
        }
    }

    /// Check if a key exists (returns true even for tombstones).
    pub fn contains(&self, key: &[u8]) -> bool {
        self.lookup(key).is_some()
    }

    /// Get the newest entry for a key, including tombstones (None if absent).
    pub fn get_entry(&self, key: &[u8]) -> Result<Option<Entry>> {
        Ok(self.lookup(key))
    }

    /// Find the newest version of `key` by seeking to its highest-sequence
    /// InternalKey and confirming the landed node shares the same user key.
    ///
    /// `new_max` sorts ahead of every real version of `key`, so the seek lands
    /// exactly on the newest stored version (or on a different user key, meaning
    /// `key` is absent).
    fn lookup(&self, key: &[u8]) -> Option<Entry> {
        let search = InternalKey::new_max(key);
        let mut iter = self.list.iter_ref();
        iter.seek(search.as_encoded());

        let found = iter.key()?;
        let ikey = InternalKey::from_encoded(found.to_vec());
        if ikey.user_key() != key {
            return None;
        }

        let value = iter.value().map(|v| v.to_vec()).unwrap_or_default();
        Some(Entry {
            key: key.to_vec(),
            value,
            sequence: ikey.sequence(),
            value_type: ikey.value_type(),
        })
    }

    /// Create an iterator over all entries in the MemTable.
    pub fn iter(&self) -> MemTableIterator<'_> {
        MemTableIterator::new(self)
    }
}

impl Default for MemTable {
    fn default() -> Self {
        Self::with_capacity(0, 4 * 1024 * 1024)
    }
}

/// Iterator over a MemTable.
pub struct MemTableIterator<'a> {
    _memtable: &'a MemTable,
    current_key: Vec<u8>,
    current_value: Vec<u8>,
    current_seq: u64,
    current_is_delete: bool,
}

impl<'a> MemTableIterator<'a> {
    fn new(memtable: &'a MemTable) -> Self {
        Self {
            _memtable: memtable,
            current_key: Vec::new(),
            current_value: Vec::new(),
            current_seq: 0,
            current_is_delete: false,
        }
    }

    #[allow(dead_code)]
    fn update_from_entry(&mut self, key: &[u8], value: &[u8], seq: u64, is_delete: bool) {
        self.current_key = key.to_vec();
        self.current_value = value.to_vec();
        self.current_seq = seq;
        self.current_is_delete = is_delete;
    }
}

impl<'a> Iterator for MemTableIterator<'a> {
    fn seek(&mut self, _key: &[u8]) {
        // Not yet implemented for new SkipList
        self.current_key.clear();
        self.current_value.clear();
        self.current_seq = 0;
        self.current_is_delete = false;
    }

    fn seek_to_first(&mut self) {
        // Not yet implemented for new SkipList
        self.current_key.clear();
        self.current_value.clear();
        self.current_seq = 0;
        self.current_is_delete = false;
    }

    fn seek_to_last(&mut self) {
        // Not implemented
    }

    fn next(&mut self) {
        // Not implemented
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
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);

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
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);

        let seq1 = memtable.put(b"key", b"value1").unwrap();
        assert_eq!(seq1, 1);

        let seq2 = memtable.put(b"key", b"value2").unwrap();
        assert_eq!(seq2, 2);

        assert_eq!(memtable.get(b"key").unwrap(), Some(b"value2".to_vec()));
    }

    #[test]
    fn test_delete() {
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);

        memtable.put(b"key", b"value").unwrap();
        memtable.delete(b"key").unwrap();

        assert_eq!(memtable.get(b"key").unwrap(), None);
    }

    #[test]
    fn test_sequence_numbers() {
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);

        assert_eq!(memtable.next_sequence(), 1);

        memtable.put(b"a", b"1").unwrap();
        assert_eq!(memtable.next_sequence(), 2);

        memtable.put(b"b", b"2").unwrap();
        assert_eq!(memtable.next_sequence(), 3);

        // Verify entries are stored correctly
        let entry_a = memtable.get_entry(b"a").unwrap().unwrap();
        assert_eq!(entry_a.key, b"a");
        assert_eq!(entry_a.value, b"1");

        let entry_b = memtable.get_entry(b"b").unwrap().unwrap();
        assert_eq!(entry_b.key, b"b");
        assert_eq!(entry_b.value, b"2");
    }

    #[test]
    fn test_empty() {
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);
        assert!(memtable.is_empty());
        assert_eq!(memtable.len(), 0);

        memtable.put(b"key", b"value").unwrap();
        assert!(!memtable.is_empty());
        assert_eq!(memtable.len(), 1);
    }

    #[test]
    fn test_get_returns_newest_version() {
        // Arrange: three versions of the same key, each a distinct node.
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);
        memtable.put(b"k", b"v1").unwrap();
        memtable.put(b"k", b"v2").unwrap();
        memtable.put(b"k", b"v3").unwrap();

        // Act
        let got = memtable.get(b"k").unwrap();

        // Assert: the highest-sequence version wins.
        assert_eq!(got, Some(b"v3".to_vec()));
        assert_eq!(memtable.len(), 3, "all versions are retained as nodes");
    }

    #[test]
    fn test_put_after_delete_revives_key() {
        // Arrange: a key that has been written then tombstoned.
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);
        memtable.put(b"k", b"v1").unwrap();
        memtable.delete(b"k").unwrap();

        // Act: write the key again with a newer sequence.
        memtable.put(b"k", b"v2").unwrap();

        // Assert: the newest version (the put) shadows the tombstone.
        assert_eq!(memtable.get(b"k").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn test_get_entry_reports_tombstone_with_sequence() {
        // Arrange: a key that is written then deleted.
        let mut memtable = MemTable::with_capacity(0, 4 * 1024 * 1024);
        memtable.put(b"k", b"v1").unwrap();
        let delete_seq = memtable.delete(b"k").unwrap();

        // Act
        let entry = memtable.get_entry(b"k").unwrap().unwrap();

        // Assert: the newest version is the tombstone, carrying its real sequence.
        assert_eq!(entry.value_type, ValueType::Delete);
        assert_eq!(entry.sequence, delete_seq);
        assert!(
            memtable.contains(b"k"),
            "contains is true even for a tombstone"
        );
    }
}
