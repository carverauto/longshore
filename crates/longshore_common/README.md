# longshore_common

The I/O layer every longshore crate is written against.

- `io`: the `Io` trait, its operations and limits, and the `io_uring`
  backend on raw system calls. A protocol crate does no I/O of its own. It
  submits operations to an `Io` and is fed their completions. Buffers and
  sockets are registered with the kernel once, and nothing is allocated
  after start-up.
- `sim`, behind the `sim` feature: a deterministic simulator that stands in
  for the kernel in tests, with virtual sockets, files and time. It is
  seeded, so a failing run replays exactly, and it can inject resets, short
  writes, partial reads, flipped bits and delays.

The `io_uring` backend needs Linux 6.1 or later. The crate builds elsewhere,
and the simulator runs there.

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_common>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
