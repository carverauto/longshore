## ADDED Requirements

### Requirement: Self-contained client
The library SHALL provide a client that starts and owns its I/O thread, can
be cloned and shared across threads, and requires no event loop, buffer
registration or token bookkeeping from the caller.

#### Scenario: First use
- **WHEN** a caller builds a client from an endpoint, a region and credentials, and stores then fetches an object
- **THEN** both calls succeed without the caller creating an `Io` or polling for completions

#### Scenario: Shutdown
- **WHEN** the last clone of a client is dropped
- **THEN** operations in progress finish or fail, the I/O thread exits and its resources are released

### Requirement: Configurable number of I/O threads
The client SHALL run on one I/O thread by default and SHALL run on a number
of independent I/O threads chosen when it is built, each with its own
connections and buffers and with no state shared between them on the data
path.

#### Scenario: Default
- **WHEN** a client is built without a thread count
- **THEN** it starts exactly one I/O thread

#### Scenario: Several threads
- **WHEN** a client is built with four threads and many operations are started at once
- **THEN** operations run on all four threads and every one completes

#### Scenario: Lent buffer stays on its thread
- **WHEN** a caller takes a buffer from a client with several threads and stores it
- **THEN** the operation runs on the thread that owns the buffer and the body is not copied

#### Scenario: Shutdown with several threads
- **WHEN** the last clone of a client with several threads is dropped
- **THEN** every I/O thread exits and its resources are released

### Requirement: Client driven by the caller's event loop
The library SHALL provide a client that starts no thread, exposes a file
descriptor that is readable when the client has work to do, and does that
work in a call that never blocks, so that a caller with one event loop per
core can run a client on each core.

#### Scenario: Driven by polling
- **WHEN** a caller builds such a client, starts a store and a fetch, and calls `turn` each time the descriptor is readable
- **THEN** both operations complete and no thread was started by the library

#### Scenario: Nothing to do
- **WHEN** `turn` is called while the client has no completions to process
- **THEN** it returns at once

### Requirement: Bucket addressing
The store SHALL address a bucket either in the host name or in the request
path, as configured, and SHALL sign requests for the form it sends.

#### Scenario: Bucket in the path
- **WHEN** the store is configured for path addressing and stores then fetches an object against a store that accepts only that form
- **THEN** both requests are accepted

### Requirement: Async and blocking calls
Every operation SHALL be available as an async method that works under any
executor and as a blocking method.

#### Scenario: Two executors
- **WHEN** the same test suite runs under two different async executors
- **THEN** every operation completes under both

#### Scenario: Plain thread
- **WHEN** a thread with no async runtime calls the blocking form of fetch
- **THEN** the call returns the object

### Requirement: Zero-copy path
The client SHALL lend registered buffers to callers so that a body is sent
and received without being copied, and SHALL prevent access to a buffer
while an operation holds it.

#### Scenario: Lent buffer round trip
- **WHEN** a caller takes a buffer, fills it, stores it and receives the buffer back with the result
- **THEN** the body was not copied by the client between the caller's write and the socket

#### Scenario: Future dropped mid-operation
- **WHEN** a caller drops the future of a fetch while the kernel is still receiving into its buffer
- **THEN** the buffer returns to the pool only after the operation has finished on the I/O thread

### Requirement: Copying convenience path
The client SHALL offer forms of store and fetch that take and return byte
slices, at the cost of one copy into or out of a registered buffer.

#### Scenario: Slice in, bytes out
- **WHEN** a caller stores a byte slice and fetches the key into a vector
- **THEN** the fetched bytes equal the stored bytes

### Requirement: Streaming transfer
The client SHALL upload a stream of unknown length through multipart upload,
up to the store's multipart limits, and download an object of any size
through ranged fetches, each using a bounded number of buffers.

#### Scenario: Upload larger than the buffer pool
- **WHEN** a caller writes more bytes to an upload stream than all registered buffers hold together, in a stream that stays within the store's part size and part count limits
- **THEN** the upload completes and the object equals what was written

#### Scenario: Buffer below the minimum part size
- **WHEN** a caller opens an upload stream on a client whose buffer size is below the store's 5 MiB minimum part size
- **THEN** opening the stream fails with an error that names the minimum part size (this scenario depends on the open part size decision in design.md)

#### Scenario: Upload stream dropped
- **WHEN** an upload stream is dropped before it is finished
- **THEN** its multipart upload is aborted

#### Scenario: Download larger than the buffer pool
- **WHEN** a caller reads an object larger than all registered buffers together to the end
- **THEN** every byte is delivered in order

### Requirement: Back pressure
The client SHALL make callers wait when no buffer or operation slot is free,
and SHALL NOT queue work without bound or allocate after it is built.

#### Scenario: More callers than slots
- **WHEN** more operations are started concurrently than the client has operation slots
- **THEN** the extra calls wait and complete as slots free up, and memory use does not grow

### Requirement: Builds everywhere, runs on Linux
The crate SHALL compile on platforms other than Linux, and building a client
there SHALL fail with an error stating that the platform is unsupported.

#### Scenario: Documentation build
- **WHEN** the crate's documentation is built on a non-Linux host
- **THEN** the build succeeds
