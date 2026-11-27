# longshore_tcp

[![crates.io](https://img.shields.io/crates/v/longshore_tcp.svg)](https://crates.io/crates/longshore_tcp)
[![docs.rs](https://img.shields.io/docsrs/longshore_tcp)](https://docs.rs/longshore_tcp)
[![License](https://img.shields.io/crates/l/longshore_tcp.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

TCP connections for protocols that run straight over TCP or Unix-domain
sockets: replication, messaging, a database's own wire protocol, its
clients and servers.

`Net` is a fixed set of connections and listeners, driven from the
caller's own loop. It borrows the I/O backend on each call and never
waits, so connections, files (`longshore_file`) and the object storage
client share one backend and one thread. `KernelTransport` carries the
set over the kernel's sockets through the `Io` trait: on `io_uring`, on
the portable backend and in the simulator. Nothing allocates after the
set is built, except the TLS handshake.

## What it gives

- Frames delivered whole, with a fixed-size header that carries the
  length, or raw bytes as they come.
- The set lends each frame in the memory that received it. A program can
  write the frame to a direct-I/O log and send it on to other connections
  from that same memory, so its bytes move once.
- Three receive modes per connection: `Rx::Ring`, a buffer ring shared by
  every connection, for many connections with small frames; `Rx::Direct`,
  each body straight into block-aligned frame memory, for large frames and
  frames that are logged; and `Rx::ZeroCopy`, which also takes away the
  kernel's copy (Linux 7.0, `CAP_NET_ADMIN`, and a card that splits
  headers from payload).
- The caller owns time: the set reports its next deadline with
  `Net::next_deadline`, and the caller passes the time to `Net::poll`.
- Half-close on request, Unix-domain stream sockets, and TLS 1.3 with any
  of longshore's providers in the ring and direct receive modes.
- `check::run_all`, the suite every transport passes, which a real kernel
  can run too.

## Example

An echo server for frames with a four-byte length prefix. It sends each
frame back from the memory that received it:

```rust
use longshore_common::io::{Completions, Io, Limits};
use longshore_tcp::{
    Config, ConnOpts, Event, Framing, HeaderFraming, KernelTransport, ListenConfig, Net, Sizes,
};
use std::net::SocketAddr;

/// `make` builds the backend from the limits planned for the set.
fn echo<IO: Io>(make: impl FnOnce(Limits) -> IO, addr: SocketAddr) -> Option<()> {
    // No connections of our own, 64 accepted, one listener.
    let (limits, config) = Config::plan(Limits::BASE, Sizes::new(0, 64, 1)).ok()?;
    let mut io = make(limits);
    let mut net = Net::new(KernelTransport::new(&config), config, &limits);
    let framing = Framing::Header(HeaderFraming::length_prefix(1 << 20).ok()?);
    let how = ListenConfig::new(ConnOpts::new(framing));
    let _listener = net.listen(&mut io, addr, how).ok()?;
    let mut done = Completions::new(256);
    loop {
        io.tick(true, &mut done).ok()?;
        while let Some(completion) = done.pop() {
            let _ours = net.on_completion(&mut io, completion);
        }
        while let Some(event) = net.next_event() {
            let Event::Readable(conn) = event else { continue };
            while let Some(frame) = net.next_frame(&mut io, conn).ok()? {
                // Into frame memory if it is not there, then sent from it.
                let held = net.hold(&mut io, frame).ok()?;
                let _queued = net.send_lent(conn, &held);
                net.release(held);
            }
        }
        let now = io.now_ns();
        net.poll(&mut io, now);
    }
}
```

## Features

| Feature | What it turns on |
|---|---|
| `aws-lc` (default; `tls` is the same) | TLS 1.3, with AWS-LC sealing and opening records |
| `ring` | TLS 1.3 with ring; no AWS-LC in the build |
| `rustls` | TLS 1.3 entirely from rustls |

With `default-features = false` and no provider, connections are plain.
The providers turn on `tls-core`; a manifest does not name it on its
own.

## Platforms

`Rx::Ring` and `Rx::Direct` run on Linux 6.1 or later, on the portable
backend and in the simulator. `Rx::ZeroCopy` needs Linux 7.0, the ring
backend, `CAP_NET_ADMIN` and a card queue with header and data split;
anywhere else, building a backend with a receive area refuses and names
what is missing.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::tcp`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead. The crates are
released together: depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_tcp>
- Guide: [TCP connections](https://github.com/carverauto/longshore/blob/main/docs/tcp.md):
  framing, receive modes, frame memory, TLS, sizing
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
