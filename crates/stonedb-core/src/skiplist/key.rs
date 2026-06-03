//! Key Comparator Trait
//!
//! Provides pluggable key comparison for SkipList.
//! This allows different key ordering semantics (e.g., bytewise, InternalKey with sequence numbers).

use std::cmp::Ordering;

/// Trait for comparing keys in the SkipList.
///
/// This allows the SkipList to work with any key type that can be compared.
/// Different comparators can implement different ordering semantics.
pub trait KeyComparator: Clone + Send + Sync {
    /// Compare two keys.
    ///
    /// Returns:
    /// - `Ordering::Less` if `lhs < rhs`
    /// - `Ordering::Equal` if `lhs == rhs`
    /// - `Ordering::Greater` if `lhs > rhs`
    fn compare_key(&self, lhs: &[u8], rhs: &[u8]) -> Ordering;

    /// Check if two keys represent the same user key.
    ///
    /// For InternalKey encoding, this ignores the sequence number suffix
    /// and compares only the user key portion.
    fn same_key(&self, lhs: &[u8], rhs: &[u8]) -> bool;
}

/// Bytewise comparator that compares keys as raw bytes.
///
/// This is the simplest comparator - just uses standard byte ordering.
#[derive(Default, Debug, Clone, Copy)]
pub struct BytewiseComparator;

impl BytewiseComparator {
    pub fn new() -> Self {
        Self
    }
}

impl KeyComparator for BytewiseComparator {
    #[inline]
    fn compare_key(&self, lhs: &[u8], rhs: &[u8]) -> Ordering {
        lhs.cmp(rhs)
    }

    #[inline]
    fn same_key(&self, lhs: &[u8], rhs: &[u8]) -> bool {
        lhs == rhs
    }
}

/// Test-only comparator for validating fixed suffix key semantics.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct FixedLengthSuffixComparator {
    suffix_len: usize,
}

#[cfg(test)]
impl FixedLengthSuffixComparator {
    fn new(suffix_len: usize) -> Self {
        Self { suffix_len }
    }
}

#[cfg(test)]
impl KeyComparator for FixedLengthSuffixComparator {
    #[inline]
    fn compare_key(&self, lhs: &[u8], rhs: &[u8]) -> Ordering {
        if lhs.len() < self.suffix_len {
            panic!(
                "cannot compare key with {} bytes (suffix_len={}): {:?}",
                lhs.len(),
                self.suffix_len,
                lhs
            );
        }
        if rhs.len() < self.suffix_len {
            panic!(
                "cannot compare key with {} bytes (suffix_len={}): {:?}",
                rhs.len(),
                self.suffix_len,
                rhs
            );
        }

        let lhs_prefix_len = lhs.len() - self.suffix_len;
        let rhs_prefix_len = rhs.len() - self.suffix_len;
        let lhs_prefix = &lhs[..lhs_prefix_len];
        let rhs_prefix = &rhs[..rhs_prefix_len];

        match lhs_prefix.cmp(rhs_prefix) {
            Ordering::Equal => {
                let lhs_suffix = &lhs[lhs_prefix_len..];
                let rhs_suffix = &rhs[rhs_prefix_len..];
                lhs_suffix.cmp(rhs_suffix)
            }
            other => other,
        }
    }

    #[inline]
    fn same_key(&self, lhs: &[u8], rhs: &[u8]) -> bool {
        if lhs.len() < self.suffix_len || rhs.len() < self.suffix_len {
            return false;
        }
        let lhs_prefix_len = lhs.len() - self.suffix_len;
        let rhs_prefix_len = rhs.len() - self.suffix_len;
        lhs[..lhs_prefix_len] == rhs[..rhs_prefix_len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bytewise_comparator() {
        let cmp = BytewiseComparator::new();

        assert_eq!(cmp.compare_key(b"a", b"a"), Ordering::Equal);
        assert_eq!(cmp.compare_key(b"a", b"b"), Ordering::Less);
        assert_eq!(cmp.compare_key(b"b", b"a"), Ordering::Greater);

        assert!(cmp.same_key(b"key1", b"key1"));
        assert!(!cmp.same_key(b"key1", b"key2"));
    }

    #[test]
    fn test_fixed_suffix_comparator() {
        let cmp = FixedLengthSuffixComparator::new(8);

        // Same user key with different suffixes (8-byte suffix: 7 nulls + 1 value)
        let key1_v1: &[u8] = b"key1\x00\x00\x00\x00\x00\x00\x00\x01"; // 12 bytes
        let key1_v2: &[u8] = b"key1\x00\x00\x00\x00\x00\x00\x00\x02"; // 12 bytes
        let key2_v1: &[u8] = b"key2\x00\x00\x00\x00\x00\x00\x00\x01"; // 12 bytes

        // Same prefix, different suffix - compare_key should show ordering
        assert_eq!(cmp.compare_key(key1_v1, key1_v2), Ordering::Less);
        assert!(cmp.same_key(key1_v1, key1_v2));

        // Different user keys
        assert_eq!(cmp.compare_key(key1_v1, key2_v1), Ordering::Less);
        assert!(!cmp.same_key(key1_v1, key2_v1));
    }

    #[test]
    fn test_fixed_suffix_comparator_panics_on_short_key() {
        let cmp = FixedLengthSuffixComparator::new(8);

        // Key too short should panic
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cmp.compare_key(b"short", b"key2")
        }));
        assert!(result.is_err());
    }
}
