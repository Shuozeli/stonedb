//! Entry Types
//!
//! Represents a key-value operation in the LSM tree.

use std::fmt;

/// Value type distinguishes different operation types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ValueType {
    /// Regular key-value pair
    Value = 0x1,
    /// Deletion (tombstone)
    Delete = 0x0,
}

impl ValueType {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x1 => Some(ValueType::Value),
            0x0 => Some(ValueType::Delete),
            _ => None,
        }
    }
}

/// An entry in the LSM tree
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The key (user key, without sequence/type suffix)
    pub key: Vec<u8>,
    /// The value (empty for deletes)
    pub value: Vec<u8>,
    /// Sequence number (higher = newer)
    pub sequence: u64,
    /// Type of entry (Value or Delete)
    pub value_type: ValueType,
}

impl Entry {
    /// Create a new Value entry
    pub fn new_value(key: Vec<u8>, value: Vec<u8>, sequence: u64) -> Self {
        Self {
            key,
            value,
            sequence,
            value_type: ValueType::Value,
        }
    }

    /// Create a new Delete entry (tombstone)
    pub fn new_delete(key: Vec<u8>, sequence: u64) -> Self {
        Self {
            key,
            value: Vec::new(),
            sequence,
            value_type: ValueType::Delete,
        }
    }

    /// Check if this is a deletion
    pub fn is_delete(&self) -> bool {
        self.value_type == ValueType::Delete
    }

    /// Check if this entry is for the given user key
    pub fn is_key(&self, key: &[u8]) -> bool {
        self.key == key
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Entry({:?}, seq={}, {:?})",
            String::from_utf8_lossy(&self.key),
            self.sequence,
            self.value_type
        )
    }
}
