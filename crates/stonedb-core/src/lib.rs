//! StoneDB Core
//!
//! Pure LSM-tree data structures with no I/O dependencies.
//! This is the foundation that storage and engine layers build on.
//!
//! # Architecture
//!
//! ```text
//! stonedb-core (this crate)
//! ├── skiplist.rs    - SkipList implementation (O(log n) sorted structure)
//! ├── memtable.rs   - MemTable (wraps SkipList, assigns sequence numbers)
//! ├── key.rs        - InternalKey encoding (user_key | seq | type)
//! ├── entry.rs      - Entry types (Value, Delete)
//! └── iterator.rs   - Iterator trait
//!
//! Build order:
//! 1. Entry, Key, Error (no dependencies)
//! 2. SkipList (core data structure)
//! 3. Iterator trait
//! 4. MemTable (combines SkipList + sequence numbers)
//!
//! # Timeline (optional)
//!
//! Enable `timeline` feature for async event recording:
//! ```toml
//! stonedb-core = { features = ["timeline"] }
//! ```
//! ```

// Re-export for convenience
pub use entry::{Entry, ValueType};
pub use error::{Error, Result};
pub use iterator::Iterator;
pub use key::InternalKey;
pub use memtable::MemTable;
pub use skiplist::SkipList;

#[cfg(feature = "timeline")]
pub use timeline::{emit, Observable, TimelineEvent};

// Modules
mod entry;
mod error;
mod iterator;
mod key;
mod memtable;
mod skiplist;

#[cfg(feature = "timeline")]
mod timeline;
