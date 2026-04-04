//! Timeline System
//!
//! Records internal events to a JSONL file for debugging and replay.
//! Uses Tokio's mpsc channel for async-native event streaming.
//!
//! # Feature Flag
//!
//! Enable with `timeline` feature:
//! ```toml
//! [dependencies]
//! stonedb-core = { features = ["timeline"] }
//! ```
//!
//! When disabled, all timeline operations are no-ops (zero overhead).

#[cfg(feature = "timeline")]
use tokio::sync::mpsc;

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// Timeline event types
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TimelineEvent {
    // SkipList events
    #[serde(rename = "skiplist_insert")]
    SkipListInsert {
        key: String,
        value: String,
        level: usize,
        result: String,
    },
    #[serde(rename = "skiplist_get")]
    SkipListGet { key: String, found: bool },
    #[serde(rename = "skiplist_contains")]
    SkipListContains { key: String, found: bool },
    #[serde(rename = "skiplist_lower_bound")]
    SkipListLowerBound { key: String, found: bool },

    // MemTable events
    #[serde(rename = "memtable_put")]
    MemTablePut {
        key: String,
        value: String,
        seq: u64,
        size: u64,
    },
    #[serde(rename = "memtable_delete")]
    MemTableDelete { key: String, seq: u64, size: u64 },
    #[serde(rename = "memtable_get")]
    MemTableGet {
        key: String,
        found: bool,
        seq: Option<u64>,
    },
    #[serde(rename = "memtable_contains")]
    MemTableContains { key: String, found: bool },
}

/// Observable event emitter using Tokio channel
#[cfg(feature = "timeline")]
pub struct Observable {
    sender: mpsc::Sender<TimelineEvent>,
}

#[cfg(feature = "timeline")]
impl Observable {
    /// Create a new Observable with specified buffer size
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<TimelineEvent>) {
        let (sender, receiver) = mpsc::channel(buffer);
        (Self { sender }, receiver)
    }

    /// Emit an event (async, fire-and-forget if buffer full)
    pub async fn emit(&self, event: TimelineEvent) {
        let _ = self.sender.send(event).await;
    }

    /// Try to emit without waiting (non-blocking)
    pub fn try_emit(&self, event: TimelineEvent) -> bool {
        self.sender.try_send(event).is_ok()
    }
}

/// No-op Observable when timeline feature is disabled
#[cfg(not(feature = "timeline"))]
pub struct Observable;

#[cfg(not(feature = "timeline"))]
impl Observable {
    pub fn new(_buffer: usize) -> (Self, std::future::Ready<()>) {
        (Self, std::future::ready(()))
    }

    /// No-op when timeline is disabled
    pub async fn emit(&self, _: TimelineEvent) {}

    /// No-op when timeline is disabled
    pub fn try_emit(&self, _: TimelineEvent) -> bool {
        true
    }
}

/// Global sequence counter for ordering events
static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Get next sequence number
fn next_seq() -> u64 {
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Get current timestamp in microseconds
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

/// Get current thread name
fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("unknown")
        .to_string()
}

/// Emit a timeline event to the observable
#[cfg(feature = "timeline")]
pub async fn emit(observable: &Observable, event: TimelineEvent) {
    observable.emit(event).await;
}

/// No-op emit when timeline feature is disabled
#[cfg(not(feature = "timeline"))]
pub async fn emit(_observable: &Observable, _event: TimelineEvent) {
    // No-op - zero overhead
}

/// Serialize an event to JSON for writing to file
#[cfg(feature = "timeline")]
pub fn serialize_event(event: &TimelineEvent) -> serde_json::Result<String> {
    let mut map = serde_json::Map::new();

    // Add metadata fields
    map.insert("ts".to_string(), serde_json::json!(now_micros()));
    map.insert("thread".to_string(), serde_json::json!(thread_name()));
    map.insert("seq".to_string(), serde_json::json!(next_seq()));

    // Serialize the event and merge its fields
    let event_obj = serde_json::to_value(event)?;
    if let serde_json::Value::Object(event_map) = event_obj {
        map.extend(event_map);
    }

    serde_json::to_string(&serde_json::Value::Object(map))
}

/// Base64 encode bytes for JSON serialization
#[cfg(feature = "timeline")]
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut result = String::new();

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = chunk.get(1).copied().unwrap_or(0) as usize;
        let b2 = chunk.get(2).copied().unwrap_or(0) as usize;

        result.push(ALPHABET[b0 >> 2] as char);
        result.push(ALPHABET[((b0 & 0x03) << 4) | (b1 >> 4)] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[((b1 & 0x0F) << 2) | (b2 >> 6)] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[b2 & 0x3F] as char);
        } else {
            result.push('=');
        }
    }

    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_observable_disabled() {
        let (obs, _fut) = Observable::new(100);
        // Should compile and work as no-op
        assert!(obs.try_emit(TimelineEvent::SkipListGet {
            key: "test".to_string(),
            found: true,
        }));
    }
}
