# longshore_mailbox

[![crates.io](https://img.shields.io/crates/v/longshore_mailbox.svg)](https://crates.io/crates/longshore_mailbox)
[![docs.rs](https://img.shields.io/docsrs/longshore_mailbox)](https://docs.rs/longshore_mailbox)
[![License](https://img.shields.io/crates/l/longshore_mailbox.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

Bounded lock-free mailboxes that carry requests from any thread to an I/O
core and answers back. The shareable longshore client runs on them.

A caller takes a slot, sends a `Copy` message that names it, and waits on
the slot for the answer. The I/O core wakes when a message arrives,
through `Io::waker()` and `Op::AwaitWake`, so it sleeps in the same
completion queue as its other I/O. Building the mailbox allocates the
queue, the slots and room for waiting callers; the waiters' queue grows
only if more callers wait than it has room for.

## What it gives

- A bounded multi-producer, single-consumer queue of `Copy` messages. A
  full queue hands the message back without allocating.
- `stop` takes one place reserved for it: messages sent before it arrive
  first, and later sends are refused. `close` is atomic with `send`: it
  keeps the answers already written, and answers each slot sent but not
  yet answered with the closed outcome.
- `Pool` and `SlotWaiter`: the caller slots, and waiting on one mailbox or
  on many.
- `Cells`, where the core writes each answer; detaching a slot returns a
  tag the caller attached, after the slot is free again.
- `CallFuture`, one request and its answer as a `Future`, and `block_on`,
  which runs it on a plain thread.
- `mailbox_parked`, a mailbox with no `Io`, whose receiver parks its
  thread.
- `sync`: the standard synchronization types, swapped for `loom`'s under
  `--cfg longshore_model` so the model checker can explore every
  interleaving.

## Example

A core that doubles numbers, and a caller that asks it:

```rust
use longshore_common::io::Token;
use longshore_mailbox::{block_on, mailbox_parked, Config, Recv};

// Eight queued messages, four caller slots, room for four waiting callers.
let config = Config::new(8, 4, 4, Token::new(1));
let (tx, mut rx) = mailbox_parked::<(u32, u64), Option<u64>>(config);

let core = std::thread::spawn(move || {
    while let Recv::Message((slot, n)) = rx.recv_parked() {
        // A tag comes back only for a slot its caller detached.
        let _tag = rx.answer(slot, Some(n * 2));
    }
});

// The call answers `None` if the mailbox closes first.
assert_eq!(block_on(tx.call(|slot| (slot, 21), None)), Some(42));
drop(tx);
core.join().unwrap();
```

A core that also does I/O builds its mailbox with `mailbox(&mut io,
config)` instead, arms it, and drains it when its wake token completes.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::mailbox`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead. The crates are
released together: depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_mailbox>
- Guide: [The I/O layer](https://github.com/carverauto/longshore/blob/main/docs/io.md)
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
