## Context
The low-level store is single-threaded and not `Send` across its event loop.
Most callers are on other threads, often inside an async runtime.

## Goals / Non-Goals
- Goals: a client that is `Send + Sync + Clone`; no dependency on a
  particular async runtime; the zero-copy path stays reachable; back pressure
  instead of unbounded queues
- Non-Goals: replacing the low-level API, or running the I/O loop on a
  caller's runtime

## Decisions
- The client owns one I/O thread per `Client`. Callers submit through a
  bounded queue of fixed-size request messages, and the thread is woken
  through an eventfd registered with the ring. Completion wakes the caller's
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
- Streaming upload fills lent buffers and sends each as a part, keeping a
  fixed number in flight. Streaming download issues ranged fetches ahead of
  the reader by a fixed window. Both are bounded by the buffer pool.
- A stream that is dropped before completion aborts its multipart upload.
- Cancellation: dropping a future detaches the caller but lets the operation
  finish on the I/O thread, so a buffer is never returned while the kernel
  may still write to it.
- Errors are one public enum. Low-level outcomes that are not errors, such as
  `NotFound`, are mapped to `Option` or a dedicated variant per method.

## Risks / Trade-offs
- A queue hop between threads costs time on small requests. The overhead
  benchmark measures it, and the low-level API remains for callers that
  cannot afford it.
- One I/O thread per client is the model. Callers that want more build more
  clients; sharding by key is left to them.

## Open Questions
- Whether the async methods should also be offered as a `Stream` and
  `AsyncRead`/`AsyncWrite` from a specific ecosystem crate behind a feature.
  Proposed: start with the crate's own lending types and add adapters behind
  features once a user asks for one.
