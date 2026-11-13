# longshore

longshore is an object storage client written in Rust. It is built to move a
lot of data with little CPU: one thread on Linux `io_uring`, all memory
allocated at start-up, and object bodies sent to the network without being
copied. It talks to Amazon S3, to S3-compatible stores, and to Google Cloud
Storage.

The design follows the TigerBeetle talk
[The Fastest Object Storage Client in the World](https://youtu.be/NJrqmoNpVGg).

## Status

This is a library under development. It is not published as a crate, and it
is meant to become part of a larger project.

| Area | State |
|---|---|
| S3: `store`, `store_no_clobber`, `fetch`, ranged `fetch`, `list`, `erase` | Works against Amazon S3 and Linode Object Storage, over HTTP and HTTPS |
| Multipart upload: begin, store part, complete, abort | Works against Amazon S3, Linode Object Storage and Google Cloud Storage. Each part is one registered buffer |
| Google Cloud Storage | Works against a real bucket, with a bearer token |
| Azure Blob Storage | Request signing only, checked against the Azurite emulator. The store cannot use it yet |
| TLS 1.3 | `rustls` does the handshake. After that, records are sealed and opened in this repository with AES-GCM from AWS-LC |
| Checksums | CRC-64/NVME for S3, CRC-32C for Google. A fetch is verified when the store returns a checksum, and both stores do |
| Credentials | Long-lived keys, temporary keys with a session token, or a bearer token. Read from a file and replaced when the file changes, once the store has accepted the new ones |

## How it works

Everything runs on one thread, driven by `io_uring` completions. There are no
locks and no async runtime. Sockets and buffers are registered with the
kernel once.

Buffers, connection slots and operation slots are sized from the
configuration and allocated when the client is built. Nothing is allocated
after that, and the tests run under an allocator that aborts if something
is. The TLS handshake is the exception, because `rustls` allocates.

An object body goes from the caller's registered buffer to the network card
with `SEND_ZC`. Requests are signed over the head only, so a body is never
run through a cryptographic hash.

DNS, HTTP, the TLS record layer and the store's retry logic are written
against an `Io` trait and do no I/O themselves. The same code runs on the
real kernel and in `longshore::sim`, a simulator with virtual sockets, files and
time. The simulator is seeded, so a failing run replays exactly, and it can
inject resets, short writes, partial reads, flipped bits and delays.

Timers fire at intervals the client chooses. It refreshes DNS, retires
connections and rereads credentials on its own schedule, whatever a server
suggests.

## Layout

The library is one crate, `longshore`, in `crates/longshore`.

| Module | Contents |
|---|---|
| `io` | The `Io` trait, the `io_uring` backend on raw system calls, and the allocation guard |
| `sim` | The simulator, behind the `sim` feature |
| `dns` | A-record resolver over UDP |
| `http` | HTTP/1.1 client with a connection pool and chunked decoding, a stream that is plain or TLS, and a server used by the tests |
| `tls` | TLS 1.3 record layer and the `rustls` handshake wrapper |
| `checksum` | CRC-64/NVME, CRC-32C, base64 |
| `crypto` | SHA-256 and HMAC-SHA256 |
| `store` | The store: operations, SigV4 and Azure Shared Key signing, retries, listing, credentials |

`crates/longshore-bench` has the benchmarks and the programs that check the
real kernel path. It is not published.

`fuzz/` has fuzz targets for the parsers. `bench/results/` has every
measurement with the conditions it was taken under, and `bench/README.md`
explains how to run the benchmarks and summarise repeated runs.

## Requirements

- Linux 6.1 or later, on x86-64 or AArch64. Older kernels are not supported.
- A recent stable Rust toolchain.

The code builds on macOS and the simulator tests run there. Anything that
uses `io_uring` has to be cross-compiled and run on a Linux host.

## Build and test

```sh
cargo test --workspace          # unit, simulation and property tests
cargo clippy --workspace --all-targets -- -D warnings
./scripts/test.sh               # the above, then the real-kernel checks
```

`scripts/test-linux.sh` cross-compiles the check programs for
`aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl`, copies them to
the hosts listed in `~/.config/longshore/test-hosts`, and runs them. That file
has one `<rust-target> <ssh-destination>` per line. If it is missing, the
real-kernel checks are skipped with a warning.

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

s3_uring selftest                         # store, conditional store, fetch, range, erase
s3_uring put --conns 64 --secs 10         # throughput, 8 MiB objects by default
s3_uring get --conns 64 --secs 10
s3_uring multipart                        # three 8 MiB parts in parallel, then an abort
s3_uring list some/prefix/
```

For Google Cloud Storage, set `S3_VENDOR=gcs` and
`S3_ENDPOINT=storage.googleapis.com`, and give `S3_BEARER_TOKEN` in place of
the keys.

The API is low level. The caller owns the event loop, gives the store
registered buffers, submits each operation with a token, and collects
outcomes with `poll`. The simulation tests in `crates/longshore/tests/store/` are
the shortest complete examples.

## Measurements

All of these use 8 MiB objects and one client thread. GB means 10^9 bytes.
Each row names its file in `bench/results/`, which has the full conditions.

### Against Amazon S3

Run from a 32-core Graviton4 instance with a 100 Gbit/s network card, in the
same region as the bucket (`m14-s3-aws-us-east-1.md`).

| Request | Result | Limit |
|---|---|---|
| PUT, plain HTTP, 192 connections | 12.2 GB/s, median of five runs | The instance's network card |
| PUT with a CRC-64 checksum on every body | 12.2 GB/s, median of five runs | The network card |
| GET, 64 connections | 4.0 GB/s | The client's core |
| HTTPS PUT, 64 connections | 2.9 GB/s | The client's core |
| HTTPS GET, 64 connections | 2.5 GB/s | The client's core |

S3 gives each upload connection about 80 MB/s, so it takes around 150
connections to fill the card. Filling it keeps about three of the machine's
cores busy. Most of that is kernel network work on other cores. The
client's thread uses about a fifth of a core for plain PUT and about
0.6 of a core with checksums. Graviton4 is the slowest processor we measured
at AES, so the HTTPS rows are a low point for TLS.

### Between two machines

The client on one DGX Spark (Arm Cortex-X925) sends to a second one over
200 Gbit/s Ethernet. The peer is a small program of ours that discards what
it receives, so these figures show what the client costs and say nothing
about any store. Medians of five runs (`m15-dgx-repeated-with-machine-cpu.md`).

| Request | Result | Limit | Cores busy on the client machine |
|---|---|---|---|
| PUT, plain HTTP | 24.5 GB/s | The link | 2.5 |
| PUT with CRC-64 checksums | 14.7 GB/s | The client's core | 1.5 |
| GET, plain HTTP | 6.4 GB/s | The client's core | 1.5 |
| HTTPS PUT | 5.8 GB/s | The client's core | 1.3 |
| HTTPS PUT, faster AES routine | 7.6 GB/s | The client's core | 1.3 |
| HTTPS GET, faster AES routine | 4.1 GB/s | The client's core | 1.5 |

The "faster AES routine" rows use a build made with
`scripts/build-fast-aes-aarch64.sh`. AWS-LC has an 8-way AES-GCM routine for
Arm that it only uses on processors it recognises, and the Cortex-X925 is
not one of them. The script fixes AWS-LC's processor capabilities when it is
compiled. A binary built that way crashes on a processor without those
instructions, so it is for hardware you know. x86-64 and Graviton do not
need it.

### AES-GCM by processor

HTTPS speed follows the processor's AES speed: in a profile of an HTTPS
upload, two thirds of the client's core is inside the cipher. Sealing TLS
records, one core, in GiB/s (`m13-aes-gcm-by-cpu.md`):

| Processor | GiB/s |
|---|---|
| AMD EPYC 9R05, Zen 5 (AWS `m8azn`) | 25.6 |
| AMD EPYC 9R45, Zen 5 (AWS `c8a`) | 22.2 |
| Intel Xeon 6975P-C (AWS `c8i`) | 16.2 |
| Arm Cortex-X925, faster routine (DGX Spark) | 12.2 |
| AMD EPYC 9R14, Zen 4 (AWS `c7a`) | 10.4 |
| Arm Cortex-X925, default build | 7.8 |
| AWS Graviton4 (AWS `c8g`) | 6.1 |

HTTPS upload has not been measured end to end on the AMD or Intel
processors.

### Compared with the talk

The talk's figures are from its slides. Its hardware differs from ours and
the slides do not describe the method in detail, so read this as a rough
placement.

| Measurement | Talk | Here |
|---|---|---|
| PUT, plain HTTP | 25.2 GB/s per core | 24.5 GB/s, with our 200 Gbit/s link full |
| PUT with CRC-64 checksums | 4.3 GB/s, the speed of its checksum | 14.7 GB/s |
| HTTPS PUT | No end-to-end figure. 7.4 GB/s for the cipher alone | 7.6 GB/s end to end |
| GET | 4.7 GB/s | 6.4 GB/s |
| CRC-64 alone | 4.3 GB/s | 88 GB/s, and about 30 GB/s inside a PUT |
| AES-GCM alone | 7.4 GB/s | 6.5 to 27 GB/s, depending on the processor |

The talk reports "per core". Our plain PUT fills the core the client runs
on, and about 1.3 cores of work at 16 connections and 1.6 at 32 run on
other cores, most of it in the kernel. The slides do not say whether the
talk counts that work, so we cannot tell which of our numbers matches
theirs.

Request rates for small operations are in `m9-s3-ops-linode.md` and
`m14-s3-aws-us-east-1.md`. In both, the store's latency sets the rate and
the client is mostly idle.

## Limits

- Plain PUT fills every link we have had, 200 Gbit/s between our own
  machines and 100 Gbit/s to S3. Where the client itself stops is unknown.
- x86-64 has only been measured on loopback and on a rate-limited instance.
- TLS keys are never updated in place. A connection is retired after 2^23
  records and the next request handshakes again. If a server updates its
  keys in the middle of a connection, the request is retried on a new one.
- If a server sends extra bytes after a complete response and they arrive
  in a later read, the client does not notice. It does catch them when they
  arrive in the same read.
- One `store` or `fetch` moves at most one buffer. A larger object goes up
  as a multipart upload and comes down as ranged fetches, and the caller
  drives both.
- A multipart upload that is neither completed nor aborted keeps its parts
  in the store. A bucket rule that expires incomplete uploads is the backstop.
- Azure Blob Storage cannot be used through the store yet, and its signing
  has not been tried against a real account.
- HTTP/1.1, IPv4 and A records only.
- Most measurements were taken on a single afternoon each. The rows marked
  as medians were run five times, and the others once.

## License

Apache License 2.0. See `LICENSE`.
