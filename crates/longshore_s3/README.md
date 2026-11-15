# longshore_s3

The object store of longshore: operations against Amazon S3, S3-compatible
stores and Google Cloud Storage.

There are two ways in.

- `client` is the one most programs want. A `Client` is a handle any thread
  can share, over one or more I/O threads. Its calls are futures that can
  also block, and bodies live in buffers it lends.
- `s3` is the store itself, single-threaded and written against the `Io`
  trait of `longshore_common`. The caller owns the event loop, submits each
  operation with a token and collects outcomes by polling.

It signs requests with SigV4, retries what can be retried, and reads
credentials from long-lived keys, temporary keys with a session token or a
bearer token. The `io_uring` backend needs Linux 6.1 or later.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_s3>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
