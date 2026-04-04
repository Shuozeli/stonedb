# gRPC + FlatBuffers API

## Purpose

The API layer provides **network access** to the LSM tree engine via gRPC with FlatBuffers for efficient serialization.

## Note: Consider Simpler Alternatives First

As debated, gRPC + FlatBuffers adds complexity. Consider:
- **Unix domain sockets + binary protocol** for local access
- **Prost (protobuf)** instead of FlatBuffers if you need schema evolution
- **Raw TCP** if you control both client and server

**Start simple, add complexity only when needed.**

## RocksDB Client Interface

For reference, RocksDB does NOT have a network API built-in. Clients use:
- **RocksDB as library** (in-process)
- **RocksDB binding via rocksdbj** (Java) or similar
- **Tribbler/KVRocks** for network access

StoneDB's gRPC API is a design choice to add network access.

## FlatBuffers Schema

### Key Types

```fbs
// proto/key.fbs
namespace stonedb;

struct InternalKey {
    user_key: [ubyte];
    sequence: uint64;
    value_type: byte;           // 1=Put, 2=Delete
}

struct KeyValue {
    key: InternalKey;
    value: [ubyte];
}
```

### Request/Response Messages

```fbs
// proto/api.fbs
namespace stonedb;

table PutRequest {
    key: [ubyte];
    value: [ubyte];
    sync: bool = false;
}

table PutResponse {
    status: Status;
    sequence: uint64;
}

table GetRequest {
    key: [ubyte];
    snapshot_sequence: uint64 = 0;  // 0 = latest
}

table GetResponse {
    status: Status;
    value: [ubyte];
    sequence: uint64;
}

table DeleteRequest {
    key: [ubyte];
    sync: bool = false;
}

table DeleteResponse {
    status: Status;
    sequence: uint64;
}

table ScanRequest {
    start_key: [ubyte];
    end_key: [ubyte];           // Exclusive, empty = no limit
    limit: int32 = 100;
    snapshot_sequence: uint64 = 0;
}

table ScanResponse {
    status: Status;
    entries: [KeyValue];
    count: int32;
    has_more: bool;
}

table BatchRequest {
    entries: [BatchEntry];
    sync: bool = false;
}

table BatchEntry {
    key: [ubyte];
    value: [ubyte];
    entry_type: byte;           // 1=Put, 2=Delete
}

table BatchResponse {
    status: Status;
    sequence: uint64;
    success_count: int32;
}

table CompactRequest {
    start_key: [ubyte];
    end_key: [ubyte];
}

table CompactResponse {
    status: Status;
    files_removed: int32;
    files_added: int32;
}

enum Status : byte {
    OK = 0,
    NOT_FOUND = 1,
    ERROR = 2,
    // Consider richer error codes:
    // MEMTABLE_FULL = 3,
    // SSTABLE_CORRUPTED = 4,
    // DISK_FULL = 5,
}
```

## Improved Error Model

```fbs
// Better error handling with actionable codes
enum ErrorCode : byte {
    OK = 0,
    NOT_FOUND = 1,
    INVALID_ARGUMENT = 2,
    MEMTABLE_FULL = 3,
    SSTABLE_CORRUPTED = 4,
    DISK_FULL = 5,
    COMPACTION_IN_PROGRESS = 6,
    INTERNAL_ERROR = 99,
}

table Error {
    code: ErrorCode;
    message: string;
    retryable: bool;
}

table GetResponse {
    error: Error;
    value: [ubyte];
    sequence: uint64;
}
```

## Service Definition

```protobuf
// proto/service.proto
syntax = "proto3";

package stonedb;

import "api.fbs";

service StoneDB {
    // Single operations
    rpc Put(PutRequest) returns (PutResponse);
    rpc Get(GetRequest) returns (GetResponse);
    rpc Delete(DeleteRequest) returns (DeleteResponse);

    // Batch operations
    rpc Batch(BatchRequest) returns (BatchResponse);

    // Scans
    rpc Scan(ScanRequest) returns (ScanResponse);

    // Streaming scans
    rpc ScanStream(ScanRequest) returns (stream ScanResponse);

    // Admin
    rpc Compact(CompactRequest) returns (CompactResponse);

    // Snapshots
    rpc CreateSnapshot(CreateSnapshotRequest) returns (CreateSnapshotResponse);
    rpc ReleaseSnapshot(ReleaseSnapshotRequest) returns (ReleaseSnapshotResponse);
}
```

## Server Implementation (Tonic)

```rust
use tonic::{Request, Response, Status};
use flatbuffers::FlatBufferBuilder;

pub struct StoneDBService {
    db: Arc<Database>,
}

#[tonic::async_trait]
impl stonedb::StoneDB for StoneDBService {
    async fn get(
        &self,
        request: Request<GetRequest>,
    ) -> Result<Response<GetResponse>, Status> {
        let req = request.into_inner();

        let result = self.db.get(&req.key, req.snapshot_sequence);

        let mut fbb = FlatBufferBuilder::new();
        let response = match result {
            Ok(Some((value, seq))) => {
                let val_offset = fbb.create_vector(&value);
                GetResponse::create(&mut fbb, &GetResponseArgs {
                    error: None,
                    value: Some(val_offset),
                    sequence: seq,
                })
            }
            Ok(None) => {
                GetResponse::create(&mut fbb, &GetResponseArgs {
                    error: Some(fbb.create_string("not found")),
                    value: None,
                    sequence: 0,
                })
            }
            Err(e) => {
                return Err(Status::internal(e.to_string()));
            }
        };

        fbb.finish(response, None);
        Ok(Response::new(fbb.finished_data().to_vec()))
    }

    async fn scan_stream(
        &self,
        request: Request<ScanRequest>,
    ) -> Result<Response<Stream<ScanResponse>>, Status> {
        let req = request.into_inner();
        let db = self.db.clone();

        let stream = async_stream::async_stream! {
            let mut iter = db.scan(
                &req.start_key,
                if req.end_key.is_empty() { None } else { Some(&req.end_key) },
                req.limit as usize,
                req.snapshot_sequence,
            );

            while let Some(entry) = iter.next().await {
                let mut fbb = FlatBufferBuilder::new();
                // Build response
                yield build_scan_response(&mut fbb, entry);
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
}
```

## Transport Options

### Unix Domain Socket (Recommended for Local)

```rust
use tokio::net::UnixListener;
use tokio::io::AsyncReadExt;

pub async fn start_uds_server(db: Arc<Database>, path: &str) -> Result<()> {
    let listener = UnixListener::bind(path)?;

    loop {
        let (socket, _) = listener.accept().await?;
        let db = db.clone();

        tokio::spawn(async move {
            handle_connection(socket, db).await;
        });
    }
}
```

### TCP + TLS (For Distributed)

```rust
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub async fn start_tcp_tls_server(
    db: Arc<Database>,
    cert: &Path,
    key: &Path,
) -> Result<()> {
    let config = load_tls_config(cert, key)?;
    let acceptor = TlsAcceptor::from(std::sync::Arc::new(config));

    let listener = TcpListener::bind("0.0.0.0:5555").await?;

    loop {
        let (socket, addr) = listener.accept().await?;
        let stream = acceptor.accept(socket).await?;
        let db = db.clone();

        tokio::spawn(handle_tls_connection(stream, db, addr));
    }
}
```

## Client

```rust
pub struct StoneDBClient {
    channel: Channel,
}

impl StoneDBClient {
    pub async fn put(&self, key: &[u8], value: &[u8], sync: bool) -> Result<u64> {
        let mut fbb = FlatBufferBuilder::new();
        let req = PutRequest::create(&mut fbb, &PutRequestArgs {
            key: Some(fbb.create_vector(key)),
            value: Some(fbb.create_vector(value)),
            sync,
        });
        fbb.finish(req, None);

        let response = self.stub.put(Request::new(fbb.finished_data().into())).await?;
        let resp = GetResponse::get_root(&response.into_inner());

        if resp.error().is_some() {
            return Err(resp.error().unwrap().into());
        }

        Ok(resp.sequence())
    }

    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut fbb = FlatBufferBuilder::new();
        let req = GetRequest::create(&mut fbb, &GetRequestArgs {
            key: Some(fbb.create_vector(key)),
            snapshot_sequence: 0,
        });
        fbb.finish(req, None);

        let response = self.stub.get(Request::new(fbb.finished_data().into())).await?;
        let resp = GetResponse::get_root(&response.into_inner());

        if resp.error().is_some() {
            return Err(resp.error().unwrap().into());
        }

        if resp.value().is_none() {
            return Ok(None);
        }

        Ok(Some(resp.value().unwrap().to_vec()))
    }
}
```

## Backpressure and Flow Control

### Server-Side

```rust
impl StoneDBService {
    async fn scan_with_backpressure(
        &self,
        request: ScanRequest,
    ) -> Result<Response<ScanResponse>, Status> {
        // Limit batch size to prevent memory exhaustion
        let batch_size = std::cmp::min(request.limit, 1000) as usize;

        // Check client cancellation
        if request.rx.is_cancelled() {
            return Err(Status::cancelled("client cancelled"));
        }

        // Process batch
        let entries = self.db.scan_batch(
            &request.start_key,
            request.end_key.as_deref(),
            batch_size,
        )?;

        Ok(Response::new(ScanResponse { entries, has_more: true }))
    }
}
```

### Client-Side

```rust
impl StoneDBClient {
    async fn scan_paginated(&self, start_key: &[u8]) -> Result<()> {
        let mut next_key = start_key.to_vec();
        let batch_size = 100;

        loop {
            let response = self.scan(next_key, batch_size).await?;

            // Process batch
            for entry in &response.entries {
                process(entry)?;
            }

            if !response.has_more {
                break;
            }

            next_key = response.entries.last().unwrap().key.user_key.to_vec();
        }

        Ok(())
    }
}
```

## Key Files

| File | Purpose |
|------|---------|
| `proto/key.fbs` | Key type schemas |
| `proto/api.fbs` | Request/response schemas |
| `proto/service.proto` | gRPC service definition |
| `api/service.rs` | Tonic service implementation |
| `api/client.rs` | Client wrapper |
| `api/codec.rs` | FlatBuffers encoding/decoding |
| `api/server.rs` | Server setup and configuration |
| `api/transport.rs` | UDS, TCP, TLS transport |

## Implementation Notes

- **Start with Prost (protobuf)** - simpler tooling, good Rust support
- **Unix domain sockets first** - lower latency, simpler than TCP
- **Rich error codes** - map LSM errors to actionable client responses
- **Pagination/cursors** - avoid unbounded scan responses
- **Cancellation support** - check `rx.is_cancelled()` in streaming
- **Connection pooling** - reuse channels for better performance

## Status

**Not started** - Can be designed early, implemented after core engine works.
