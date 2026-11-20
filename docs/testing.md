# Building, testing and running against a store

## Requirements


- Linux 6.1 or later, on x86-64 or AArch64. Older kernels are not supported.
- A recent stable Rust toolchain.

The simulator, unit, property, and portable `kqueue` backend and client tests
run directly on macOS. Anything that uses `io_uring` (or Linux `epoll`
real-kernel checks) has to be cross-compiled and run on a Linux host.

## Build and test


```sh
cargo test --workspace          # unit, simulation and property tests
cargo clippy --workspace --all-targets -- -D warnings
./scripts/test-model.sh         # the client's hand-offs, model checked
./scripts/test.sh               # the tests and the model, then the real-kernel checks
```

`scripts/test-linux.sh` cross-compiles the check programs for
`aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl`, copies them to
the hosts listed in `~/.config/longshore/test-hosts` (or
`$LONGSHORE_TEST_HOSTS_FILE`), and runs them. That file has one
`<rust-target> <ssh-destination>` per line. If it is missing or lists zero
hosts, the script exits non-zero and reports that `io_uring` was not exercised.

`scripts/test-model.sh` builds `longshore_mailbox` and `longshore_s3` with their
locks, atomics and thread parking replaced by
[loom](https://github.com/tokio-rs/loom)'s, and runs the queue, slot, and client
hand-offs between callers and I/O threads in every order loom reaches with one
or two forced thread switches. A wake-up that could be lost shows up there as a
thread left waiting. loom is only in that build.

`client_faults`, one of the check programs, makes a running client's rings
fail and tears clients down with work in flight. It is built with the
`fault-injection` feature, which is off everywhere else.

Property tests use [Hegel](https://hegel.dev). The fuzz targets run with
`cargo fuzz` from `fuzz/`.

## Trying it against a store


`s3_uring` shows how to drive the library, and it is also the self-test and
the benchmark. Its source is in `crates/longshore-bench/src/bin/s3_uring/`. On a
Linux host:

```sh
export S3_ENDPOINT=s3.us-east-1.amazonaws.com S3_REGION=us-east-1 S3_BUCKET=my-bucket
export S3_ACCESS_KEY=... S3_SECRET_KEY=...
export S3_SESSION_TOKEN=...               # only with temporary keys
export S3_TLS=1                           # HTTPS; omit for plain HTTP
export S3_PATH_STYLE=1                     # bucket in the path: /bucket/key on the endpoint
export S3_FAMILY=prefer6                  # 4 (the default), 6, prefer6 or prefer4
export S3_PREFER_LARGE_SEGMENTS=2         # prefer large-segment addresses, using at least 2

s3_uring selftest                         # store, conditional store, fetch, range, erase
s3_uring put --conns 64 --secs 10         # throughput, 8 MiB objects by default
s3_uring get --conns 64 --secs 10
s3_uring put --conns 128 --threads 4 --cpu 2 --cpu-step 2   # four independent clients, pinned to CPUs 2, 4, 6, 8
s3_uring multipart                        # three 8 MiB parts in parallel, then an abort
s3_uring objects                          # stat, metadata, copy, delimiter listing, batch erase
s3_uring list some/prefix/

exact_s3                                  # put, head and get: what the store honours and returns
```

For Google Cloud Storage, set `S3_VENDOR=gcs` and
`S3_ENDPOINT=storage.googleapis.com`, and give `S3_BEARER_TOKEN` in place of
the keys.

The store's own API is low level. The caller owns the event loop, gives the
store registered buffers, submits each operation with a token, and collects
outcomes with `poll`. The simulation tests in `crates/longshore_s3/tests/store/` are
the shortest complete examples.
