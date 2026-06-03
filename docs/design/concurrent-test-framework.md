# Concurrent Test Framework Design

## Overview

A Rust crate for testing concurrent code with **controlled interleaving** and **dynamic capture**. Unlike traditional concurrent tests that rely on timing and luck, this framework allows tests to intercept thread execution at defined checkpoints, capture state, and decide whether to block or proceed.

## Key Design Principles

1. **No global state**: Each `Checkpoint` owns its own state and hook - no shared controller that causes test pollution
2. **`#[cfg(test)]` interception**: In production builds, checkpoints are zero-cost no-ops
3. **No thread IDs in captures**: User's hook decides based on value alone
4. **Memory safe**: Cloned values cross thread boundaries safely (`T: Clone + Send + 'static`)
5. **Timeout fail-safe**: Threads blocked longer than `timeout_ms` will panic with a clear message

## Core Concept

Threads execute code and reach **checkpoints** - named points in the code where the test can intercept. At each checkpoint:

1. The thread provides a value (type `T`) representing its current state
2. The test's hook is called with this value (as a debug-formatted string)
3. The hook decides:
   - **CaptureAndBlock**: Store a clone of the value, then block the thread
   - **BlockWithoutCapture**: Block the thread without capturing
   - **Proceed**: Let the thread continue immediately
4. Multiple threads can wait at the same checkpoint and are released together

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         Test                                │
│                                                              │
│  let checkpoint = Checkpoint::new("name", 5000, |value_str| {│
│      // Decision based on value                              │
│      if value_str.contains("important") {                   │
│          Decision::CaptureAndBlock                           │
│      } else {                                               │
│          Decision::Proceed                                   │
│      }                                                      │
│  });                                                        │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ Arc<Checkpoint<T>>
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                     Checkpoint<T>                           │
│  - name: &'static str                                      │
│  - blocking: BlockingState (AtomicBool + Mutex + timeout)  │
│  - captures: CaptureList<T> (cloned values)                │
│  - hook: Option<Box<Hook>>                                  │
│                                                              │
│  Thread 1 ──► wait() ──► BLOCKED (if Decision blocks)     │
│  Thread 2 ──► wait() ──► BLOCKED                         │
│  Thread 3 ──► wait() ──► PROCEEDS (if Decision = Proceed) │
└─────────────────────────────────────────────────────────────┘
```

## Production vs Test Build

### Production Build (`#[cfg(not(test))]`)

```rust
impl<T: Clone + Send + 'static> Checkpoint<T> {
    pub fn new<F>(name: &'static str, hook: F) -> Arc<Self>
    where F: FnMut(String) -> Decision + Send + 'static {
        Arc::new(Self { /* no-op state */ })
    }

    pub fn wait<F: FnOnce() -> T>(&self, provide_value: F) -> T {
        provide_value()  // Zero-cost passthrough
    }

    pub fn release_all(&self) {}  // No-op
    pub fn reset(&self) {}        // No-op
    pub fn waiting_count(&self) -> usize { 0 }
}
```

### Test Build (`#[cfg(test)]`)

Full interception with blocking and capture.

## API

### Checkpoint::new

```rust
pub fn new<F>(name: &'static str, timeout_ms: u64, hook: F) -> Arc<Self>
where
    F: FnMut(String) -> Decision + Send + 'static;
```

Creates a new checkpoint with a user-provided hook. The hook receives a debug-formatted string of the value and returns a `Decision`.

`timeout_ms` is the maximum time a thread will block before panicking. This prevents tests from hanging forever if `release_all()` is forgotten.

### wait

```rust
pub fn wait<F: FnOnce() -> T>(&self, provide_value: F) -> Option<T>
```

- Calls `provide_value()` to get the value
- Clones the value and passes debug string to hook
- Based on Decision:
  - `CaptureAndBlock`: Stores clone, blocks until `release_all()`
  - `BlockWithoutCapture`: Blocks until `release_all()`
  - `Proceed`: Returns immediately with `None`
- Returns `Some(value)` if released with CaptureAndBlock, `None` otherwise

### release_all

```rust
pub fn release_all(&self)
```

Releases all threads blocked at this checkpoint.

### reset

```rust
pub fn reset(&self)
```

Resets the checkpoint state (clears captures, resets blocking flag).

### waiting_count

```rust
pub fn waiting_count(&self) -> usize
```

Returns number of threads currently waiting at this checkpoint.

### captures

```rust
pub fn captures(&self) -> Vec<T>
```

Returns all captured values at this checkpoint (clones of values passed to `wait()` when Decision was CaptureAndBlock).

## Decision Enum

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Capture value clone and block thread until released
    CaptureAndBlock,
    /// Block thread without capturing
    BlockWithoutCapture,
    /// Proceed without blocking or capturing
    Proceed,
}
```

## Usage Example

```rust
#[test]
fn test_concurrent_cas_retry() {
    // Create checkpoint with conditional hook
    let checkpoint = Checkpoint::new("cas_retry", |value: String| {
        // Only capture and block for values with expected=0
        if value.contains("expected: 0") {
            Decision::CaptureAndBlock
        } else {
            Decision::Proceed
        }
    });

    let checkpoint_clone = checkpoint.clone();
    let handle = thread::spawn(move || {
        // This will block and capture
        checkpoint_clone.wait(|| CasState { expected: 0, actual: 1 })
    });

    // Wait for thread to block
    while checkpoint.waiting_count() == 0 {
        thread::sleep(Duration::from_millis(1));
    }

    // Inspect captured value
    let captures = checkpoint.captures();
    assert_eq!(captures.len(), 1);
    assert_eq!(captures[0].expected, 0);

    // Release the blocked thread
    checkpoint.release_all();

    handle.join().unwrap();
}
```

## Memory Safety

1. **T: Clone** - We clone the value so the original stays in the calling thread while a copy is stored for the test
2. **T: Send** - The cloned value crosses thread boundaries (from worker thread to test thread)
3. **T: 'static** - No lifetime issues with the stored clones
4. **T: Debug** - Required to format value for hook inspection

## Thread Safety

- `BlockingState` uses `AtomicBool` for release flag and `Mutex<usize>` for waiting count
- `CaptureList<T>` uses `Mutex<Vec<T>>` for thread-safe storage
- All state is protected by proper synchronization primitives

## Comparison with Old Design

| Aspect | Old Design | New Design |
|--------|-----------|------------|
| Controller | Global singleton | Per-checkpoint |
| Test pollution | Yes (shared global state) | No (each checkpoint isolated) |
| Hook storage | Global HashMap | Stored in Checkpoint |
| Thread IDs in captures | Yes | No |
| Production overhead | Minimal (AtomicBool check) | Zero-cost (no-op) |
| cfg(test) | No | Yes - full interception stripped |

## Crate Structure

```
concurrent-test-framework/
├── Cargo.toml
└── src/
    └── lib.rs          // All code in single file for simplicity
```

## Future Enhancements

- [x] Timeout on block (fail-fast instead of hang forever)
- [ ] Scoped checkpoints (automatically release on drop)
- [ ] Checkpoint groups (release all in group)
- [ ] Async support via same API
