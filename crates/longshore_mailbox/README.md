# longshore_mailbox

Bounded lock-free mailboxes between I/O cores and caller waiter slots for `longshore`.

- `sync`: standard synchronization primitives swapped for `loom` under `--cfg longshore_model`, including a tracked `UnsafeCell`.
- Lock-free bounded MPSC queue for `Copy` messages, reserving one extra slot for in-band `stop`, with `Io::waker()` + `Op::AwaitWake` integration, `park` fallback without `Io`, and atomic `close` vs `send`.
- Generic `Pool`, `SlotWaiter`, `Cells`, `CallFuture`, and `block_on` for waiting on one or many mailboxes and returning caller-attached tags after freeing slots.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. The crates are released together,
so depend on them at the same version.

- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
