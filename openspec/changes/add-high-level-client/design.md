## Context
The low-level store is single-threaded and not `Send` across its event loop.
Most callers are on other threads, often inside an async runtime.

## Goals / Non-Goals
- Goals: a client that is `Send + Sync + Clone`; no dependency on a
  particular async runtime; the zero-copy path stays reachable; back pressure
  instead of unbounded queues; more than one core when the caller asks for it
- Non-Goals: replacing the low-level API, or depending on a caller's
  runtime. A caller's event loop may drive a client, through a file
  descriptor and one call, with no code of that runtime in this crate

## Decisions
- The client owns a fixed number of I/O threads, set when it is built. The
  default is one. Each thread is a complete single-threaded store with its
  own ring, connections, buffers and resolver, and the threads share
  nothing. More threads are for work one core cannot carry: HTTPS and
  downloads are bound by the client's core, at about 5 and 4 GB/s on
  Graviton4, and eight independent clients side by side gave 34.5 GB/s of
  HTTPS upload against 5.0 for one. A plain upload already fills a
  300 Gbit/s card from one thread, so more threads add nothing there.
  Alternative considered: one ring shared by several threads. Rejected
  because it brings locks and shared state onto the data path, and the
  simulator could no longer replay a run exactly.
- A client can also be built with no thread of its own, for a caller that
  already runs one event loop per core. That client is not `Send`. It
  gives the caller a file descriptor that becomes readable when the client
  has work to do, and a `turn` call that does that work without blocking.
  The caller registers the descriptor with its own reactor and calls
  `turn` when it fires. Operations are the same async methods, and their
  futures complete inside `turn`. Nothing crosses a thread, so there is no
  queue hop and a request stays on the core that asked for it.
  The descriptor is the ring's own: a ring set up for deferred task work
  is readable when it has completions to deliver.
  Alternative considered: I/O threads beside the caller's cores. Works
  with the threaded client as it is, and costs a thread and a queue hop
  for every request.
- The store's bucket addressing is a setting: the bucket in the host name,
  as today, or in the path, which self-hosted S3-compatible stores commonly
  expect. Signing covers the path as sent.
- The builder takes the number of threads and, optionally, the CPU each is
  pinned to. On a machine with more than one NUMA node the threads belong
  on the network card's node: off it, one thread's plain upload fell from
  about 19 GB/s to about 4.
- The number of threads is fixed for the life of a client. A caller that
  needs more for a while, such as a restore, builds a second client for
  that job and drops it afterwards.
- An operation runs on one thread from start to finish. A new operation
  goes to the thread with the fewest in progress, or, if that thread has
  no free operation slot, to any open thread that has one. A lent buffer
  belongs to the thread that registered it, so an operation that carries a
  lent buffer runs on that buffer's thread, and `client.buffer()` hands out
  the first free buffer on any open thread. The parts of one streamed upload may
  run on different threads, since each part is its own request. Beginning
  and completing the upload are ordinary operations.
- Limits given to the builder (connections, buffers, operations) are per
  thread, so memory grows with the thread count and is still fixed at build.
- Callers submit to a thread through a bounded queue of fixed-size request
  messages, and the thread is woken through an eventfd registered with its
  ring. Completion wakes the caller's
  `Waker`, or unparks the thread for blocking calls.
  Alternative considered: integrate with a specific runtime's reactor.
  Rejected because it ties the crate to that runtime and puts its scheduler
  on the data path.
- Buffers are lent, not shared. `client.buffer().await` gives exclusive
  ownership of one registered buffer as a guard that returns it on drop.
  While an operation holds a buffer the caller cannot touch it, which the
  type system enforces by moving the guard into the operation and handing it
  back with the result.
- When no buffer or operation slot is free, calls wait. Nothing is queued
  without bound and nothing is allocated to hold the backlog.
- Streaming upload fills lent buffers and sends them as multipart parts,
  keeping a fixed number in flight. How buffers map to parts is under Open
  Questions. Streaming download issues ranged fetches ahead of the reader by
  a fixed window. Both are bounded by the buffer pool.
- A stream that is dropped before completion aborts its multipart upload.
- Cancellation: dropping a future detaches the caller but lets the operation
  finish on the I/O thread, so a buffer is never returned while the kernel
  may still write to it.
- Errors are one public enum. Low-level outcomes that are not errors, such as
  `NotFound`, are mapped to `Option` or a dedicated variant per method.
- An upload stream sends each part as exactly one registered buffer, so
  each part is one zero-copy send. Opening an upload stream fails with a
  clear error when the client's buffer size is below the store's 5 MiB
  minimum part size, and the stream fails with a clear error before it
  would exceed 10,000 parts. A streamed object is therefore capped at
  10,000 times the buffer size.

## Risks / Trade-offs
- A queue hop between threads costs time on small requests. The overhead
  benchmark measures it, and the low-level API remains for callers that
  cannot afford it.
- Every thread opens its own connections and resolves names itself, so a
  store sees the client's connection count multiplied by its threads.
- Threads do not rebalance. A thread whose operations are slow keeps them,
  and new work goes elsewhere.
- A caller that is itself one thread per core can skip all of this and
  give each of its threads a low-level store. Nothing in the low-level API
  changes.

## Open Questions
- Whether polling the ring's descriptor is enough to drive a client on
  every supported kernel, or an eventfd registered with the ring is needed.
  To be settled by the real-kernel test in task 1.6 on Linux 6.1.
- Whether the async methods should also be offered as a `Stream` and
  `AsyncRead`/`AsyncWrite` from a specific ecosystem crate behind a feature.
  Proposed: start with the crate's own lending types and add adapters behind
  features once a user asks for one.
