## Why
A program with one I/O thread per core still has to pass messages between
cores. longshore solved this once, inside the S3 client: a bounded queue of
fixed-size requests into an I/O thread, a wake through the thread's ring,
and a slot the caller waits on, all checked with loom. It is written
against the S3 request and cannot be used for anything else.

UltraGraphDB passes messages between its partitions through channels that
each core polls on a one-millisecond timer, so a message waits up to a
millisecond. It is moving its I/O to longshore
(https://github.com/carverauto/longshore/issues/40) and needs the same
hand-off between its own cores.

## What Changes
- A mailbox: a bounded queue of fixed-size messages into the thread that
  owns an `Io`, which wakes that thread through its backend
- Senders are refused when the queue is full; the queue never grows
- Two ways for a mailbox to end. A sender asks the receiver to stop: what
  was queued before is handled, what comes after is refused. Or the
  receiver closes, and a message is either taken before the close or
  refused, never left behind
- The answering side of the S3 client's hand-off, the slots a caller waits
  on by blocking or by a future, becomes generic over the answer, and is
  offered as building blocks as well as a finished future
- Both live in a crate of their own, `longshore_mailbox`
- The S3 client is rebuilt on it with no change to its API or behaviour.
  Its queue of callers waiting for a slot is preallocated for a number the
  builder sets, and grows beyond that as it does today
- The queue itself is checked under the model, which today leaves it to the
  standard library
- **BREAKING**: `Builder` gains a public field for the number of waiters
  to preallocate

## Impact
- Affected specs: `core-mailbox` (new)
- Affected code: `longshore_mailbox` (new), `longshore_s3` (`client`),
  `longshore` (re-export), `longshore-bench` (one copy of `block_on`),
  `scripts/test-model.sh`
- Depends on: `add-portable-io-backend`
- First principles: 1, each thread still owns its `Io` and nothing but
  messages crosses; the queue takes no lock, and answering a caller takes
  that caller's lock as it does today. 2, the queue and the slots are
  sized when they are built
- Not in this change: an executor, tasks or timers. A receiver is a state
  machine that takes messages in its own turn
