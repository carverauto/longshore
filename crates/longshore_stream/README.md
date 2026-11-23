# longshore_stream

Plain or TLS 1.3 byte stream over the `Io` trait of `longshore_common`, for both clients and servers.

- `Stream` presents one interface to callers whether a connection is plain TCP
  or protected by TLS 1.3 (`longshore_tls`).
- Plain streams pass sends, receives, and idle checks directly to `Io`.
- TLS streams (`tls` feature, enabled by default) drive the `rustls` handshake
  and protect records with AES-GCM via `longshore_tls`.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_stream>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
