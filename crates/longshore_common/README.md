# longshore_common

[![crates.io](https://img.shields.io/crates/v/longshore_common.svg)](https://crates.io/crates/longshore_common)
[![docs.rs](https://img.shields.io/docsrs/longshore_common)](https://docs.rs/longshore_common)
[![License](https://img.shields.io/crates/l/longshore_common.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

The I/O layer under every longshore crate: one `Io` trait, a backend for
the kernel's ring, a portable backend, and a simulator that tests run on.

Protocol code in longshore does no I/O itself. It submits each operation
to an `Io` with a token, and the loop that owns the `Io` hands the
completions back. The same code therefore runs on the kernel and in the
simulator, where a seed makes every run repeat exactly.

## Backends

| Backend | Runs on | What it gives |
|---|---|---|
| `io::uring::UringIo` | Linux 6.1 or later, x86-64 and AArch64 | `io_uring` through raw system calls: registered buffers and files, zero-copy sends, a buffer ring, linked chains |
| `io::posix::PosixIo` | macOS (`kqueue`) and Linux (`epoll`) | The same contract on readiness events, without registered buffers or zero-copy sends |
| `sim::SimIo` (feature `sim`) | Any target | Virtual sockets, files, disk and time, with seeded network faults, disk faults and power loss |

Each backend allocates its buffers, slots and operation tables when it is
built, from a `Limits`. Nothing allocates after that.

## What `io` holds

- `Op`, the operations: TCP and Unix-domain sockets, accept and receive
  armed once over a buffer ring, file reads and writes (direct I/O
  included), sync, reserve, punch, rename, unlink, mkdir, timeouts,
  cancels, and wake-ups from other threads.
- `Io::submit`, `Io::submit_chain` for operations that must run in order,
  and `Io::tick`, which collects completions.
- `Limits`, which sizes every table and buffer pool, and `extents` and
  `turn`, which share the pools and order a batch of completions.
- `contract`, `contract_files`, `contract_chains` and `contract_net`: the
  checks every backend passes, so a new backend or a real kernel can run
  them too.
- `alloc_guard`: the allocator the tests run under, which aborts on an
  allocation after start-up.

## Example

A timer, written once against the trait and run on the portable backend:

```rust,no_run
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{Completions, Io, Limits, Op, Token};

/// Waits one millisecond on any backend.
fn sleep_once<IO: Io>(io: &mut IO) -> std::io::Result<()> {
    let timer = Token::new(7);
    io.submit(timer, Op::Timeout { nanos: 1_000_000 })
        .expect("a free operation slot");
    let mut done = Completions::new(8);
    loop {
        io.tick(true, &mut done)?;
        while let Some(c) = done.pop() {
            if c.token() == timer {
                return Ok(());
            }
        }
    }
}

// Room for eight operations in flight, and nothing else.
let mut io = PosixIo::new(Limits::BASE.with_ops_max(8)).unwrap();
sleep_once(&mut io).unwrap();
```

## Features

| Feature | What it turns on |
|---|---|
| `sim` | The deterministic simulator, for tests |
| `fault-injection` | `io::uring::Fault`, which breaks a running ring on purpose; off in every ordinary build |

## Platforms

`UringIo` needs Linux 6.1 or later, enough `RLIMIT_MEMLOCK` for the
registered buffers, and `RLIMIT_NOFILE` of at least `sockets_max +
files_max`. `PosixIo` runs on macOS and Linux. Other targets build the
crate and run the simulator.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::io` (and `longshore::sim`). Most programs depend on
[longshore](https://crates.io/crates/longshore) instead. The crates are
released together: depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_common>
- Guide: [The I/O layer](https://github.com/carverauto/longshore/blob/main/docs/io.md)
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
