# longshore_http_client

HTTP/1.1 for the longshore object storage client, over plain TCP or TLS.

- `head` does no I/O. It writes request heads into a buffer the caller
  provides and parses response heads out of one, with explicit limits and no
  allocation.
- `client` is the connection pool, with chunked decoding and a stream that
  is plain or TLS.
- `server` is a scriptable object server used by tests and benchmarks.

Both `client` and `server` are written against the `Io` trait of
`longshore_common`, so they run the same on the kernel and in the simulator.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_http_client>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
