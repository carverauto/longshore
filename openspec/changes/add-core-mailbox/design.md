## Context
`longshore_s3/src/client/` holds the hand-off in about 2,250 lines. `Lane`
carries a bounded standard-library channel of `Request`, the backend's
waker, a `Pool` of operation slots with its waiters, and a `Cell` per slot
that a caller waits on with a condition variable or a stored `Waker`.
`Pending<T>` is a future with a blocking `wait`. `Pool`, `block_on` and the
switch between standard and loom primitives are generic already. `Request`,
`Finished` and the worker's dispatch are S3's.

Nine loom tests in `tests/client/model.rs` check the slots and the failure
of a lane. The channel is not swapped under the model, so the queue is not
checked. `Pool`'s waiter queue is built for as many waiters as there are
slots and can grow past that.

## Goals / Non-Goals
- Goals: a message from one core reaches another without a timer; back
  pressure; no allocation after it is built; every hand-off checked under
  the model; the S3 client's API and figures unchanged
- Non-Goals: an executor; broadcast; messages of varying size; ordering
  between different senders; one ring shared by threads

## Decisions
- A mailbox has one receiver, the thread that owns the `Io`, and any number
  of senders. A sender holds a handle that is `Send` and can be cloned.
- The message type is the caller's and is `Copy` of fixed size, as
  `Request` is. A larger payload travels in a lent buffer, as an object's
  body does today.
- A send that finds the queue full returns the message to the sender. The
  capacity is set when the mailbox is built.
- A send signals the receiver's waker. The receiver keeps one `AwaitWake`
  in flight and drains the mailbox when it completes, as the S3 worker
  does. A signal sent while no wait is in flight is kept, which the
  backends already guarantee.
- The queue is written in this crate over the primitives the model can
  swap, so the model checks it. The `unsafe` it needs stays inside this
  crate, and callers use only safe calls.
  Alternative considered: keep the standard-library channel. Rejected
  because the model cannot see inside it.
- The answering side is generic over the answer: a `Pool` of slots, a cell
  per slot holding the answer, a future with a blocking `wait`. The S3
  client supplies `Finished` as the answer. A caller that needs no answer
  uses the mailbox alone.
- `Pool`'s waiter queue is bounded. A caller that cannot wait for a slot is
  told so, where today the queue would grow.
- Closing: when the receiver goes away, every sender is refused from then
  on and every caller waiting for an answer is given the closed error. The
  S3 client's shutdown message becomes this.
- In the simulator, mailboxes join several engines on one thread. A message
  is seen when the receiving engine is next ticked, so the test's order of
  ticks decides the order of delivery and a run replays.
- The S3 client keeps `Request`, the worker's dispatch, buffer lending and
  the credential counters. Only the queue, the slots and the waiting move.

## Risks / Trade-offs
- A queue written here can be wrong where the standard one is not. → It is
  small, and the model checks every interleaving up to the bound used
  today.
- The move could slow the S3 path. → Figures before and after; the types
  are generic, so the compiled code should be the same.

## Migration Plan
1. The crate with the queue, the slots and the model tests.
2. The S3 client on it; its nine model tests pass against the new crate.
3. The allocator that aborts is turned on for the threaded client's tests.

## Open Questions
None.
