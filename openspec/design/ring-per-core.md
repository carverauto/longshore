# Ring per core

The core design of longshore's I/O layer: one thread per core, each owning
one completion queue that serves its sockets, its files and its messages
from other cores. It began as the design of an object storage client and is
extended, in https://github.com/carverauto/longshore/issues/40, to carry a
whole storage service. UltraGraphDB is the first such consumer.

Parts that exist today are marked *today*. Parts that a pending change adds
carry that change's issue number.

## The rule

A core's thread owns everything it touches and shares none of it.

- One thread drives all of a core's I/O from one completion queue. No
  locks and no async runtime on the data path.
- Memory is allocated when the thread's I/O is built. Nothing allocates
  after that.
- Code that decides what to do performs no I/O. It submits operations and
  is handed their completions.
- What crosses between cores is a message of fixed size, and nothing else.

Each of the sections below is one of these four applied to a part of the
system. Where a feature could not meet them it was left out, and the last
section lists those.

## Layers

```
  core thread
  +------------------------------------------------------------+
  |  state machines         the consumer's logic, and           |
  |  (no I/O of their own)  longshore's: HTTP, DNS, TLS, S3     |
  +---------------- submit(token, op) / completions ------------+
  |  Io                     slots, pools, pending operations    |
  +------------------------------------------------------------+
  |  ring backend       |   engine                              |
  |  io_uring           |   + virtual world  (the simulator)    |
  |                     |   + POSIX world    (kqueue, epoll)    |
  +------------------------------------------------------------+
```

Everything above the `Io` line is the same code on every backend. That is
how one program runs on the kernel in production, in the simulator under a
seed, and on a developer's Mac.

## The Io boundary

*Today*, in `longshore_common::io`.

- **Operations.** `submit(token, op)` queues one operation and returns at
  once. It refuses with `Busy` when the fixed capacity is in use, which is
  the layer's back pressure.
- **Completions.** `tick(wait, &mut completions)` enters the kernel once,
  and returns the operations that finished as a token and a result. The
  result is a byte count, zero, or a negated Linux error number, on every
  backend.
- **Tokens.** A token is the caller's 64-bit number. Each component takes
  a range of the token space by its high bits, and passes on a completion
  that is not in its range. Components therefore share one queue without
  knowing of each other.
- **Buffers.** The `Io` owns every buffer the kernel touches. A caller
  names a buffer by number and reads or writes it through the `Io`. An
  operation in flight owns its buffer until its completion is delivered.
  Callers need no `unsafe`.
- **Slots.** Sockets and files live in tables of fixed size and are named
  by slot. In the ring they are registered files, so the process holds no
  descriptor for them.

The contract (#41):

- Every accepted operation gets exactly one final completion.
- A cancel names the operation in flight under its token at the moment the
  cancel is submitted.
- A close gives every other operation in flight on its socket its final
  completion, without waiting for the peer.
- An operation that stays armed (#44) marks each completion but its last
  as "more will follow". It can end at any time, and its last completion
  may still carry a result, so a caller handles the result and then arms
  again.

One set of tests of this contract runs against every backend.

## The turn

A core's thread runs one loop for as long as it lives.

```
  loop {
      take completions            from the last tick
      step the state machines     each handles what is its own
      submit                      what they now want done
      tick(wait)                  one entry to the kernel
  }
```

One entry to the kernel carries everything the turn produced: socket sends,
file writes and their syncs, and re-armed receives. One return brings
everything that finished. A request that arrives on a socket, reads from
disk, and answers, never leaves its thread and never waits on a queue
between threads.

The thread sleeps only in `tick`, and only when nothing is ready. A message
from another core, a timer and a completion all end that sleep the same
way, as a completion.

There are no futures, wakers or tasks on this path. A component is a state
machine with three entry points: start, a completion that is its own, and a
tick of its timers. longshore's HTTP client, resolver and S3 store are
written this way *today*, and a consumer's logic is written the same way.

## Backends

**The ring** (*today*). `io_uring` through raw system calls, with no
dependency. The ring is set up for a single issuer with deferred task work,
which needs Linux 6.1. That is the floor for every feature: nothing may
need a newer kernel. On Linux the ring is required. If the kernel refuses
it, start-up fails and says why; there is no silent change of backend,
because the published figures would stop applying unnoticed.

**The engine and its worlds** (#41). The simulator is split in two. The
engine holds what the contract describes: pending operations, cancellation,
timers, back pressure, completion order. A world attempts one operation,
holds whatever is drawn from the seed, tells the time, and waits for
something to become ready.

- The *virtual world* is the simulator: sockets, files, faults and time
  that exist only in memory, all decided by one seed.
- The *POSIX world* attempts with non-blocking system calls and waits with
  kqueue on macOS or epoll on Linux. It is the portable backend, for
  development, tests, and hosts that are asked to run without the ring. It
  makes no performance claim.

The contract is thus implemented twice, in the ring and in the engine, and
not once per platform. The portable backend cannot drift from the
simulator, because it is the simulator's engine with a real kernel behind
it.

## Memory

- Pools are sized from `Limits` and allocated when the `Io` is built
  (*today*): large buffers, small buffers, and with #42 a pool aligned to
  the device's block size for direct I/O, and with #44 a ring of buffers
  for receives.
- Pools are registered with the kernel as a few large ranges (#42), so the
  kernel pins them once and no operation pays for it again.
- Registered memory is locked memory, charged to a limit that every ring
  of one user shares. The kernel is the judge of it. When it refuses,
  building the `Io` fails and reports the pool's size and the limit (#42).
- A body is not copied. It goes from the caller's registered buffer to the
  socket or the disk, and from them into a registered buffer (*today* for
  sockets, #42 for files).
- An idle plain connection holds no buffer. A receive that stays armed
  takes one from the buffer ring only when bytes arrive (#44). A TLS
  stream keeps two record buffers for as long as it lives.

## Files

(#42.)

- A file is opened into a slot and read and written at an offset, between
  the file and a registered buffer. The backend tracks which slots are in
  use, because the kernel would replace a file in an occupied slot without
  a word.
- File operations submitted as a chain run in order. The chain ends at the
  first member that fails or moves fewer bytes than asked, and the rest
  are cancelled. A log's write and its sync are one chain in the same
  submission as the turn's socket sends; after a short write the log
  finishes the write and submits the sync again.
- A file opened for direct I/O takes only aligned buffers, offsets and
  lengths. One check, which the ring backend and the engine both call,
  refuses anything else before the kernel or a world sees it, so a mistake
  is caught in the simulator on any host.
- Space is reserved when a file is made and given back by punching holes.
  A file can be emptied when it is opened. There is no truncate and no
  directory listing; a caller names the files it opens.
- On Linux 6.1 the kernel runs sync, reserve, punch, rename, unlink and an
  open that creates on its own worker threads. Their results still arrive
  through the one queue, but their latency is the workers'.

## Networking

- Connect, send, receive, close and zero-copy send on TCP, and UDP for the
  resolver (*today*).
- An accept that stays armed, a receive that stays armed over the buffer
  ring, socket options and Unix-domain sockets (#44).
- TLS sits above the `Io`, not in it. `Stream` is a byte stream over `Io`
  that is plain or TLS by configuration, for the connecting and the
  accepting side (#44 moves it to its own crate). TLS is a build feature;
  a plain build carries no TLS code.

## Between cores

(#45.)

Cores do not share a ring, and a core never touches another's buffers or
slots. A core that needs something from another sends it a message.

- A mailbox is a bounded queue of fixed-size messages into one core's
  thread. A send wakes that thread through its own `Io`, so the message is
  taken in the receiver's next turn, without a timer.
- A send to a full mailbox is refused. The sender decides what to do,
  which is the same back pressure `submit` gives.
- A mailbox ends in one of two ways. A sender asks the receiver to stop,
  and what was queued before is still handled. Or the receiver closes, and
  every message is either taken or refused to its sender.
- A caller outside the core threads may wait for an answer, by blocking or
  by a future. That is how the S3 client is called from other threads
  *today*, and the mailbox is that mechanism made general.

More cores add capacity by adding independent copies, not by sharing. Eight
independent clients side by side carried 34.5 GB/s of HTTPS upload against
5.0 for one (`openspec/changes/add-high-level-client/design.md`).

## Simulation

*Today* for the network, #43 for the disk.

- The virtual world draws every decision from one seed: the order
  operations complete in, delays, resets, short sends, partial receives,
  flipped bits. Time is virtual. There are no threads and no wall clock.
- A run is identified by a hash of every completion it delivered. The same
  seed gives the same hash.
- The disk keeps what was written apart from what is durable. Power can be
  lost, for the whole simulated machine, leaving each unsynced write whole,
  absent or torn. Writes can be lost or land in the wrong place, reads can
  fail, syncs can fail (#43). The model's rules are donated from
  UltraGraphDB's `SimStorage` and extended here.
- Within a simulated backend, files, sockets and time share one seed and
  one clock, so a whole turn of a core replays exactly.
- Several cores are several simulated backends on one thread, joined by
  mailboxes. Each has its own seed and clock. A run replays from those
  seeds and the order in which the test ticks them (#45).
- Hand-offs between real threads cannot be replayed. They are checked with
  loom, which explores their interleavings.
- The tests run under an allocator that aborts when anything allocates
  after start-up.

## What is left out, and why

| Left out | Why |
|---|---|
| An executor, tasks, `spawn` | Callers are state machines. An executor would put a scheduler on the data path, and its tasks would need memory after start-up. |
| A thread pool for file I/O | It brings locks and a second thread. The ring does file I/O without one, and the portable backend makes no performance claim. |
| A ring shared by threads | It brings locks and shared state, and the simulator could no longer replay a run. |
| Integration with another runtime's reactor | It ties the layer to that runtime and puts its scheduler on the data path. |
| Truncate | On the ring only from Linux 6.9. The alternatives were a higher kernel floor or a blocking call on the I/O thread. |
| Directory listing | No kernel has it on the ring. A caller names its files, by probing fixed names or keeping a list of its own. |
| A bound that refuses a waiting caller | The client's calls could not report it without a change to its API. The waiter queue is preallocated and grows beyond that. |
| A silent fallback from the ring | The published figures would stop applying without anyone noticing. |
| HTTP/2 | Object stores gain nothing from it over pooled HTTP/1.1. |

## A consumer's view

What a service built on this layer does, with UltraGraphDB as the example.

- It starts one thread per core, and each builds its own `Io` with limits
  for its sockets, files and pools.
- Its logic is state machines that take completions and submit operations.
  It takes a range of the token space, as longshore's own components do.
- A partition's turn takes its requests from sockets, writes its log and
  syncs it as one chain, reads its blocks with direct I/O, fetches from the
  object store through the same `Io`, and answers, all on its thread.
- It names its own files. Its log finds its segments by probing their
  fixed names, where it listed a directory before.
- Partitions speak to each other through mailboxes.
- Its tests run the same code in the virtual world, with network and disk
  faults from one seed.
- Its crates contain no `unsafe`. Every `unsafe` block is in longshore.

## Changes

| Issue | Change | Adds |
|---|---|---|
| #41 | `add-portable-io-backend` | Engine and worlds, kqueue and epoll, the contract |
| #42 | `add-file-operations` | Files, chains, aligned pools, registration by range |
| #43 | `add-simulated-disk` | Power loss and disk faults in the simulator |
| #44 | `add-server-networking` | Armed accept and receive, buffer ring, `longshore_stream` |
| #45 | `add-core-mailbox` | Messages between cores |
