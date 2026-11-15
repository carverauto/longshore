# longshore_dns

A DNS resolver for a fixed set of host names, driven by longshore I/O
completions.

- `codec` does no I/O. It encodes queries and parses responses, and never
  reads outside the bytes it is given.
- `resolver` drives it over the `Io` trait of `longshore_common`. It
  resolves every configured name at start-up, for A records, AAAA records or
  both, and refreshes on a fixed interval of its own. A lookup on the
  request path reads an in-memory cache and does no I/O.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_dns>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
