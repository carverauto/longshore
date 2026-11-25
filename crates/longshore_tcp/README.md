# longshore_tcp

TCP connections over the `Io` trait of `longshore_common`, for protocols
that run straight over TCP: messaging, replication, database clients and
servers.

- `Net` is a fixed set of connections and listeners driven from the caller's
  own loop. It borrows the I/O backend on each call and never waits, so
  file I/O and other clients share the same ring and thread.
- Frames are delivered whole (a fixed-size header that carries the length)
  or as raw bytes, lent in place from registered memory.
- Frame memory is block-aligned and counted: a received frame can be
  written to a direct-I/O log and sent on to other connections from where
  it was received.
- The caller owns time: `Net` reports its next deadline and is told the
  current time.
- Unix-domain stream sockets, and TLS 1.3 with any of longshore's
  providers (`aws-lc`, `ring`, `rustls`), in both receive modes.
- Nothing allocates after `Net` is built, except the TLS handshake.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. 

- Documentation: <https://docs.rs/longshore_tcp>
- Repository: <https://github.com/carverauto/longshore>
