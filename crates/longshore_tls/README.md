# longshore_tls

TLS 1.3 for the longshore object storage client.

- `record` is the record layer. It seals application data read from the
  caller's buffer into a send buffer, and opens received records into the
  caller's buffer, with AES-GCM from AWS-LC. It does no I/O and no
  allocation.
- `handshake` wraps `rustls` for setting up a connection and nothing else.
  It does no I/O either. It does allocate, which is the one exception to the
  client's rule of allocating nothing after start-up. When it finishes, its
  keys are installed in `record`.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_tls>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
