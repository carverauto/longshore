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
cargo test --workspace --exclude longshore-bench --exclude longshore_tls --no-default-features  # plain build without TLS
cargo test -p longshore_tls -p longshore_stream -p longshore_http_client -p longshore_s3 --no-default-features --features ring    # TLS through ring; likewise `rustls`
cargo test -p longshore_common --features fault-injection               # stale-readiness and fault-injection tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --exclude longshore-bench --exclude longshore_tls --all-targets --no-default-features -- -D warnings
./scripts/test-model.sh         # the client's hand-offs, model checked
./scripts/test.sh               # the tests and the model, then the real-kernel checks
cargo bench -p longshore-bench --bench <name>   # criterion microbenchmarks (see bench/README.md)
```

`scripts/test-linux.sh` runs the real-kernel checks and needs no
configuration. On Linux it builds the check programs and runs them on that
machine. Elsewhere it starts a Linux container with `docker` or `podman`
and runs itself there, on the kernel of the container host. The list of
checks is `scripts/linux-checks.sh`.

For a kernel or hardware the machine at hand does not have, it then runs
the same checks on other machines over SSH: those listed in
`scripts/test-hosts`, which is part of the repository, and those given as
parameters, each a `<rust-target> <ssh-destination>` pair:

```sh
scripts/test-linux.sh aarch64-unknown-linux-musl user@arm-host
```

If no Linux kernel can be reached, or one of those machines cannot, the
script exits non-zero and says that `io_uring` was not exercised. It reads
no environment variable and no file outside the repository.

`scripts/test-model.sh` builds `longshore_mailbox` and `longshore_s3` with their
locks, atomics and thread parking replaced by
[loom](https://github.com/tokio-rs/loom)'s, and runs the queue, slot, and client
hand-offs between callers and I/O threads in every order loom reaches with one
or two forced thread switches. A wake-up that could be lost shows up there as a
thread left waiting. loom is only in that build.

`client_faults`, one of the check programs, makes a running client's rings
fail and tears clients down with work in flight. It is built with the
`fault-injection` feature, which is off everywhere else.

Property tests use [Hegel](https://hegel.dev). The parsers have fuzz targets
in `fuzz/`, run with `cargo fuzz` through `scripts/fuzz.sh` (or `make fuzz`):
every target for 60 seconds by default, `FUZZ_SECONDS` and `FUZZ_TARGETS` to
change that, `FUZZ_SECONDS=0` to only build them. It needs a nightly toolchain
and `cargo install --locked cargo-fuzz`. The Fuzz workflow runs each target
for a minute on a pull request and for ten on `main` and every night, keeping
each target's corpus between runs and the crashing input when one fails.

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

Without a store of your own, `scripts/self-hosted-s3.sh` starts Ceph's S3
gateway or RustFS in a container and runs the programs against it from a
second container, with the bucket in the path:

```sh
scripts/self-hosted-s3.sh up ceph      # or rustfs
scripts/self-hosted-s3.sh run ceph
scripts/self-hosted-s3.sh down
```

For Google Cloud Storage, set `S3_VENDOR=gcs` and
`S3_ENDPOINT=storage.googleapis.com`, and give `S3_BEARER_TOKEN` in place of
the keys.

The store's own API is low level. The caller owns the event loop, gives the
store registered buffers, submits each operation with a token, and collects
outcomes with `poll`. The simulation tests in `crates/longshore_s3/tests/store/` are
the shortest complete examples.
