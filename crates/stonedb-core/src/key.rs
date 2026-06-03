//! InternalKey Encoding
//!
//! InternalKey = user_key | sequence_number (7 bytes, big-endian) | value_type (1 byte)
//!
//! The sequence number is stored in the high 7 bytes, and value_type in the low byte.
//! This ensures that for the same user_key, higher sequence numbers sort first.

use crate::entry::{Entry, ValueType};
use crate::error::Result;
use crate::skiplist::KeyComparator;
use std::cmp::Ordering;

/// Maximum sequence number (2^56 - 1)
pub const MAX_SEQUENCE: u64 = 0x00FFFFFFFFFFFFFF;

/// InternalKey for storage in MemTable and SSTables
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InternalKey {
    /// The encoded key bytes (user_key | sequence | type)
    pub encoded: Vec<u8>,
}

impl InternalKey {
    /// Create a new InternalKey from parts
    pub fn new(key: &[u8], sequence: u64, value_type: ValueType) -> Self {
        let mut encoded = Vec::with_capacity(key.len() + 8);
        encoded.extend_from_slice(key);
        let encoded_sequence = MAX_SEQUENCE - sequence;
        encoded.extend_from_slice(&encoded_sequence.to_be_bytes()[1..]); // 7 bytes
        encoded.push(if value_type == ValueType::Value {
            0x1
        } else {
            0x0
        });
        Self { encoded }
    }

    /// Create InternalKey for a Put operation
    pub fn new_put(key: &[u8], sequence: u64) -> Self {
        Self::new(key, sequence, ValueType::Value)
    }

    /// Create InternalKey for a Delete operation
    pub fn new_delete(key: &[u8], sequence: u64) -> Self {
        Self::new(key, sequence, ValueType::Delete)
    }

    /// Create InternalKey with maximum sequence for search
    /// Used when searching for the newest entry for a key
    pub fn new_max(key: &[u8]) -> Self {
        Self::new(key, MAX_SEQUENCE, ValueType::Value)
    }

    /// Get the user key portion
    pub fn user_key(&self) -> &[u8] {
        // Everything except last 8 bytes
        let user_key_len = self.encoded.len().saturating_sub(8);
        &self.encoded[..user_key_len]
    }

    /// Get the sequence number
    pub fn sequence(&self) -> u64 {
        let seq_type = &self.encoded[self.encoded.len() - 8..self.encoded.len() - 1];
        let mut bytes = [0u8; 8];
        bytes[1..].copy_from_slice(seq_type); // Prepend 0
        MAX_SEQUENCE - u64::from_be_bytes(bytes)
    }

    /// Get the value type
    pub fn value_type(&self) -> ValueType {
        let t = *self.encoded.last().unwrap_or(&0);
        if t == 0x1 {
            ValueType::Value
        } else {
            ValueType::Delete
        }
    }

    /// Create from encoded bytes (must be valid)
    pub fn from_encoded(encoded: Vec<u8>) -> Self {
        Self { encoded }
    }

    /// Get the encoded bytes
    pub fn as_encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Decode an InternalKey from a MemTable or SSTable
    /// This assumes the last 8 bytes contain sequence | type
    pub fn decode_from(encoded: &[u8]) -> Result<Self> {
        if encoded.len() < 8 {
            return Err(crate::error::Error::InvalidKey(format!(
                "InternalKey too short: {} bytes",
                encoded.len()
            )));
        }
        Ok(Self::from_encoded(encoded.to_vec()))
    }
}

impl PartialOrd for InternalKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for InternalKey {
    fn cmp(&self, other: &Self) -> Ordering {
        // First compare user_key
        let self_user = self.user_key();
        let other_user = other.user_key();
        match self_user.cmp(other_user) {
            Ordering::Equal => {
                // Then compare sequence (higher first, so reverse)
                let self_seq = self.sequence();
                let other_seq = other.sequence();
                other_seq.cmp(&self_seq) // Reverse: higher sequence first
            }
            other => other,
        }
    }
}

impl From<&Entry> for InternalKey {
    fn from(entry: &Entry) -> Self {
        Self::new(&entry.key, entry.sequence, entry.value_type)
    }
}

/// Length of the InternalKey trailer: 7-byte sequence + 1-byte value type.
const TRAILER_LEN: usize = 8;

/// Split an encoded InternalKey into `(user_key, trailer)`.
///
/// Defensive: keys shorter than the trailer are treated as all user key. Well
/// formed InternalKeys always carry the 8-byte trailer, so this only guards the
/// empty head sentinel.
fn split_internal(key: &[u8]) -> (&[u8], &[u8]) {
    if key.len() < TRAILER_LEN {
        (key, &[])
    } else {
        key.split_at(key.len() - TRAILER_LEN)
    }
}

/// Comparator for InternalKey-encoded keys stored in the MemTable skiplist.
///
/// Orders by user key ascending, then by the encoded 8-byte trailer ascending
/// so the newest version (highest sequence) of a user key sorts first. This mirrors
/// [`InternalKey`]'s [`Ord`] and is StoneDB's analogue of AgateDB's
/// `FixedLengthSuffixComparator(8)`: because every write carries a distinct
/// sequence, each insert is a unique immutable node and the skiplist never needs
/// to mutate an existing value.
#[derive(Debug, Clone, Copy, Default)]
pub struct InternalKeyComparator;

impl InternalKeyComparator {
    pub fn new() -> Self {
        Self
    }
}

impl KeyComparator for InternalKeyComparator {
    #[inline]
    fn compare_key(&self, lhs: &[u8], rhs: &[u8]) -> Ordering {
        let (lhs_user, lhs_trailer) = split_internal(lhs);
        let (rhs_user, rhs_trailer) = split_internal(rhs);
        match lhs_user.cmp(rhs_user) {
            // The trailer stores MAX_SEQUENCE - sequence, so newer sequences
            // have smaller encoded trailers and sort first.
            Ordering::Equal => lhs_trailer.cmp(rhs_trailer),
            other => other,
        }
    }

    #[inline]
    fn same_key(&self, lhs: &[u8], rhs: &[u8]) -> bool {
        split_internal(lhs).0 == split_internal(rhs).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_internal_key_ordering() {
        let key_a = InternalKey::new(b"a", 100, ValueType::Value);
        let key_a_200 = InternalKey::new(b"a", 200, ValueType::Value);
        let key_b = InternalKey::new(b"b", 50, ValueType::Value);

        // Higher sequence for same key should sort first
        assert!(key_a_200 < key_a);

        // Different keys sorted by key, ignoring sequence
        assert!(key_a < key_b);
    }

    #[test]
    fn test_internal_key_user_key() {
        let key = InternalKey::new(b"hello", 100, ValueType::Value);
        assert_eq!(key.user_key(), b"hello");
        assert_eq!(key.sequence(), 100);
        assert_eq!(key.value_type(), ValueType::Value);
    }

    #[test]
    fn test_internal_key_max() {
        let key = InternalKey::new_max(b"test");
        assert_eq!(key.user_key(), b"test");
        assert_eq!(key.sequence(), MAX_SEQUENCE);
    }

    #[test]
    fn test_comparator_orders_newest_version_first() {
        // Arrange: two versions of the same user key plus a later user key.
        let cmp = InternalKeyComparator::new();
        let a_v100 = InternalKey::new(b"a", 100, ValueType::Value);
        let a_v200 = InternalKey::new(b"a", 200, ValueType::Value);
        let b_v50 = InternalKey::new(b"b", 50, ValueType::Value);

        // Act + Assert: higher sequence of the same user key sorts first;
        // different user keys sort by user key regardless of sequence.
        assert_eq!(
            cmp.compare_key(a_v200.as_encoded(), a_v100.as_encoded()),
            Ordering::Less
        );
        assert_eq!(
            cmp.compare_key(a_v100.as_encoded(), b_v50.as_encoded()),
            Ordering::Less
        );
        assert!(cmp.same_key(a_v100.as_encoded(), a_v200.as_encoded()));
        assert!(!cmp.same_key(a_v100.as_encoded(), b_v50.as_encoded()));
    }

    #[test]
    fn test_comparator_matches_internal_key_ord() {
        // Arrange: the standalone comparator must agree with InternalKey::Ord.
        let cmp = InternalKeyComparator::new();
        let lhs = InternalKey::new(b"k", 7, ValueType::Value);
        let rhs = InternalKey::new(b"k", 9, ValueType::Delete);

        // Act + Assert.
        assert_eq!(
            cmp.compare_key(lhs.as_encoded(), rhs.as_encoded()),
            lhs.cmp(&rhs)
        );
    }
}
