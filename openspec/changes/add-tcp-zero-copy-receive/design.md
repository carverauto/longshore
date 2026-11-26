## Context
Measured on two DGX Sparks (kernel 7.0, ConnectX-7 `mlx5_core`, one
200 Gb/s port seen as two interfaces of about 110 Gb/s each), milestone 30:

| | GB/s a core | CPU-seconds a GB |
|---|---|---|
| Receive, ring, kernel TCP | 5.4 to 7.3 | 0.13 to 0.16 |
| Send, zero-copy, 1 MiB segments | not the limit | 0.024 to 0.030 |
| Send, copied | 5.3 to 6.1 | 0.083 to 0.186 |

Kernel TCP fills the link (198 Gb/s) with eight receiving cores at 77 %.

What `zcrx` is, from the kernel's documentation
(`Documentation/networking/iou-zcrx.rst`): a ring registers one receive
queue of one interface with a memory area and a refill ring; a multishot
`IORING_OP_RECV_ZC` on a socket then completes with offsets into the
area; the program returns each buffer through the refill ring. It needs
the ring set up with single issuer, deferred task work (both of which
longshore already uses) and 32-byte or mixed completions; a card that
splits headers from payload; and flow steering plus RSS set by the
operator so that only the wanted flows reach the queue. The area is cut
into chunks of the page size unless a larger length is asked and granted.

## Goals / Non-Goals
- Goals: receive without the kernel's copy on the ring backend; the same
  `Net` API with one addition; the same contract, tests and refusals as
  the other receive modes; numbers that decide the AF_XDP change.
- Non-Goals: a user-space TCP stack; configuring the card; `zcrx` on the
  portable backend; a second memory pool for files.

## Decisions
- **A receive mode, not a transport.** The kernel still does TCP, so
  everything above receive (states, events, close, TLS, reconnect) is the
  kernel transport's. `Rx::ZeroCopy` is chosen per listener or connection
  like the other modes.
- **Chunks take the place of ring buffers.** Ring mode already lends a
  frame that lies in one buffer and assembles one that does not. Here a
  frame in one chunk is lent the same way; the difference is the frame
  that spans chunks.
- **A spanning frame is lent as pieces.** With chunks of 4 KiB every frame
  larger than that spans, so assembling would copy everything and the
  mode would win nothing. `Net::pieces(&frame)` yields the frame's byte
  runs in order; the frame holds its chunks until `release`. `hold` moves
  it once into block-aligned frame memory for a caller that needs one run.
  - Alternatives considered: larger chunks only (`rx_buf_len`): needs
    contiguous memory and driver support the documentation calls
    optional, and a frame still starts wherever the stream puts it, so a
    block-aligned write needs the move anyway. Kept as a setting to
    measure, not as the design.
- **The log path keeps its one move.** A direct-I/O write needs a
  block-aligned run; payload arrives behind the stream's own offsets.
  Receive, `hold`, write, send to replicas is one move, as with `Direct`
  today. What zero-copy receive removes is the copy for frames consumed
  in place (queries, small writes batched by the caller, frames
  forwarded).
- **One queue a ring, one ring a thread.** A registration binds a ring to
  one receive queue. A thread's listener must therefore be reached only
  through that queue: the operator steers the listener's port (or the
  thread's address) to it. With the Spark's two interfaces a thread
  serves one of them.
- **Accounting as one structure**, as for chunks of the buffer ring: every
  chunk is in exactly one of kernel, lent with a frame, or queued for
  refill; checked after every step of the model test.
- **Exhaustion.** No free chunk: the kernel stops filling the queue and
  TCP's window closes; the program is told nothing is lost and continues
  when frames are released. A full refill ring cannot happen: it has as
  many entries as the area has chunks.
- **TLS.** Records are opened from pieces into frame memory, as from ring
  buffers today; the mode saves the kernel's copy, not the decryption's.
- **Zero-copy sends.** The measured case fits sixteen sends in flight a
  connection and a notification that comes milliseconds after the send.
  Task 4.1 traces where it waits before anything is changed; the
  candidates are a larger default segment for bulk senders and counting a
  zero-copy send as done for the queue when its first completion comes,
  holding only its memory until the notification.

## Risks / Trade-offs
- The driver may not support it on the Sparks → the spike of group 1 is
  first and is allowed to end the change.
- A queue given to one ring is taken from the kernel's RSS → documented;
  the refusal names an unsteered queue where it can be detected.
- `CAP_NET_ADMIN` is needed to register → refusal names it.
- 32-byte completions double the completion ring's memory → only when the
  mode is asked for.
- The saving may be small where the consumer touches every byte anyway
  (the copy's cache misses move to the consumer) → measured per case
  before any default changes.

## Migration Plan
None: an added mode, off unless chosen. Rollback is not choosing it.

## Open Questions
- Does `mlx5_core` in 7.0 accept the registration, and with which card
  settings (`rx-gro-hw` was the only related one found)?
- Can memory of the area also be registered as fixed buffers, so that a
  frame is forwarded from where it lies with a zero-copy send?
- Mixed completions (`IORING_SETUP_CQE_MIXED`) instead of 32-byte ones:
  from which kernel, and is it worth a second floor?
