## Context
`longshore_s3/src/client/` holds the hand-off in about 2,250 lines.

- `Lane` carries a bounded standard-library channel of `Request`, the
  backend's waker, a `Pool` of operation slots and a second `Pool` of lent
  buffers, a `Cell` per slot, and beside them the lending fields, counters
  and a closed flag.
- A `Cell` holds the answer, the caller's waker, a detached flag and the
  number of a lent buffer.
- `Pending<T>` is a future with a blocking `wait`. It waits for a slot on
  one lane, or on every lane at once when the request names no buffer.
- When a caller drops a request that is still running, the I/O thread
  later frees the buffer and then the slot.
- `Pool`, `block_on` and the switch between standard and loom primitives
  are generic already. `Pool`'s waiter queue is built for as many waiters
  as there are slots and grows past that; a test has one slot and two
  waiters.

Ending works in two ways today. Dropping the client sends a shutdown
request in band: requests queued before it are served, those after it are
refused, and the thread stops. When the ring fails, the thread closes the
lane, answers what was running, and then stays parked, without a working
`Io`, so the memory that lent buffers point into outlives them.

Ten loom tests in `tests/client/model.rs` check the slots and the failure
of a lane, in 58 seconds. The channel is not swapped under the model, so
the queue is not checked.

## Goals / Non-Goals
- Goals: a message from one core reaches another without a timer; back
  pressure; every hand-off checked under the model; the S3 client's API,
  behaviour and figures unchanged
- Non-Goals: an executor; broadcast; messages of varying size; ordering
  between different senders; one ring shared by threads; a bound on
  waiting callers that would refuse one

## Decisions

### The queue
- A mailbox has one receiver, the thread that owns the `Io`, and any number
  of senders. A sender holds a handle that is `Send` and can be cloned.
- The message type is the caller's and is `Copy` of fixed size. `Request`
  gains the derive; all its fields are `Copy` already. A larger payload
  travels in a lent buffer, as an object's body does today.
- A send that finds the queue full returns the message to the sender. The
  capacity is set when the mailbox is built.
- The queue is written in this crate over primitives the model can swap,
  so the model checks it. The `unsafe` it needs stays inside this crate,
  and callers use only safe calls.
  Alternative considered: keep the standard-library channel. Rejected
  because the model cannot see inside it.
- The switch between standard and model primitives moves here from
  `client/sync.rs` and is public, since the S3 client still needs it for
  its own counters. It gains a wrapper for a cell the model can track.

### Waking and receiving
- A send signals the receiver's waker and unparks its thread. The receiver
  normally keeps one `AwaitWake` in flight and drains the mailbox when it
  completes. A signal sent while no wait is in flight is kept, which the
  backends already guarantee. A sender handle can also wake without
  sending.
- The receiver can also wait for a message by parking, without an `Io`.
  A thread whose backend has failed uses that while it keeps its memory
  alive.
- The generic receiver needs the backend's waker and fails to build when
  the backend has none.

### Ending
- Stop is asked by a sender and travels in the queue, which keeps one
  place beyond its capacity for it. Messages queued before it are
  delivered; messages after it are refused as closed. The receiver decides
  when it has finished and then closes.
- Close is the receiver's. It is atomic with a send: a message is either
  in the queue when the receiver makes its last drain, or its send is
  refused as closed. No message is left where neither side will see it.
- When a mailbox closes, the receiver answers what it can first. Then
  every slot that was sent and has no answer is given the closed error.
  A cell records that it was sent, so closing never overwrites an answer
  and never answers a slot nobody is waiting on.

### Answers
- The answering side is generic over the answer: a `Pool` of slots, a
  cell per slot, and the waiting by blocking or by a future.
- It is offered at two levels. The building blocks are: take a slot or
  wait for one, on one pool or on several; send; poll for the answer or
  wait for it; give up. A finished future is built from them for callers
  with one mailbox and no extras. The S3 client uses the building blocks,
  because its requests wait on every lane and carry lent buffers.
- A cell carries one number that belongs to the caller, the S3 client's
  lent buffer. When the receiver answers a slot whose caller has given up,
  the mailbox frees the slot and hands that number back, and the S3 client
  returns the buffer. The order becomes slot then buffer; the model tests
  cover it.
- A pool's waiter queue is preallocated for a number set when it is built,
  and grows beyond that as today. Waiting within that number allocates
  nothing. The S3 builder gains the setting, with a default well above the
  number of slots.
  Alternative considered: refuse a caller when the queue is full. Rejected
  because the client's calls have no way to say so without a change to
  its API, and an existing test relies on waiting.

### The S3 client
- `Lane` embeds the mailbox and keeps the lending fields, the counters and
  its policy for closed lanes. `Request`, the worker's dispatch and buffer
  lending stay in the S3 crate.
- Dropping the client asks for a stop. A failed thread closes the mailbox,
  answers through it, and parks on it.
- The client's ten model tests stay where they are and are not edited. The
  new crate has model tests of its own for the queue, the slots and the
  two endings.

### Simulation
- Mailboxes can join several simulated backends on one thread. Each
  backend keeps its own clock and trace. A message is seen when the
  receiving backend is next ticked without waiting, so the test's order of
  ticks decides the order of delivery, and a run replays from the seeds
  and that order.
- That test is left out of a model build, where the queue's primitives
  work only inside the model.

## Risks / Trade-offs
- A queue written here can be wrong where the standard one is not. → It is
  small, and the model checks every interleaving up to the bound used
  today, with a test aimed at a send racing a close.
- The model tests take longer with the queue inside them. → The queue's
  own tests use two senders and one message each; the run time is recorded
  and the bound kept where it is.
- The move could slow the S3 path. → Figures before and after; the types
  are generic, so the compiled code should be the same.

## Migration Plan
1. The crate with the queue, the slots, the endings and its model tests.
2. The S3 client on it; its ten model tests pass unedited.
3. The allocator that aborts is turned on for the threaded client's tests.

## Open Questions
None.
