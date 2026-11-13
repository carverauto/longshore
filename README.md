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
| Multipart upload: begin, store part, complete, abort | Works against Amazon S3, Linode Object Storage, Google Cloud Storage and versitygw. Each part is one registered buffer |
| `stat`, stores with a content type and metadata, `copy`, `erase_many`, listing with a delimiter, presigned URLs | Work against Amazon S3, Linode Object Storage and Google Cloud Storage. `erase_many` and presigned URLs need signing keys, so they are not available with Google's bearer token |
| Google Cloud Storage | Works against a real bucket, with a bearer token |
| Azure Blob Storage | Request signing only, checked against the Azurite emulator. The store cannot use it yet |
| IPv4 and IPv6 | The resolver asks for A records, AAAA records or both, and prefers the family the configuration names. When connections to that family fail and the name has addresses in the other, it uses the other. Run over IPv6 against Linode Object Storage |
| Large-segment addresses | Optional, off by default. A store's name can resolve to addresses whose connections get different TCP segment sizes, and on a network card that does not segment in hardware the small ones cost several times more CPU per byte. With the setting on, the client measures each new connection, keeps its connections on the addresses with large segments, and asks DNS again sooner when every address is small. It can be switched while the store runs. Works on Linux 6.1 and later |
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
| `dns` | Resolver for A and AAAA records over UDP |
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
export S3_FAMILY=prefer6                  # 4 (the default), 6, prefer6 or prefer4
export S3_PREFER_LARGE_SEGMENTS=2         # prefer large-segment addresses, using at least 2

s3_uring selftest                         # store, conditional store, fetch, range, erase
s3_uring put --conns 64 --secs 10         # throughput, 8 MiB objects by default
s3_uring get --conns 64 --secs 10
s3_uring multipart                        # three 8 MiB parts in parallel, then an abort
s3_uring objects                          # stat, metadata, copy, delimiter listing, batch erase
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

All of these use 8 MiB objects, and one client thread unless a row says
otherwise. GB means 10^9 bytes.
Each row names its file in `bench/results/`, which has the full conditions.

### Against Amazon S3

One client thread, from instances in the same region as the bucket, with
the preference for large-segment addresses on. Medians of five runs, and
of eight for HTTPS PUT (`m20-preference-and-comparison.md`).

| Request | Graviton4, 300 Gbit/s card | AMD Zen 5, 200 Gbit/s card | Limit |
|---|---|---|---|
| PUT, plain HTTP | 36.7 GB/s | 22.4 GB/s | Graviton4: the network card. AMD: 22.4 GB/s is 179 Gbit/s on a 200 Gbit/s card with the client thread at 0.80, so neither the card nor the thread is clearly the limit |
| PUT with a CRC-64 checksum on every body | 16.4 GB/s | 13.3 GB/s | The client's core |
| GET | 4.4 GB/s | 7.9 GB/s | The client's core |
| HTTPS PUT, 3 MiB send batch | 5.2 GB/s | 7.3 GB/s | The client's core |
| HTTPS GET | 2.7 GB/s | 5.1 GB/s | The client's core |

Plain PUT fills the 300 Gbit/s card with half of the client's thread idle,
and keeps about four of the machine's cores busy, most of it kernel network
work on other cores. Without the preference the same instances give a
median of 23.2 and 11.8 GB/s for plain PUT, and runs differ by 45% or more,
because S3's name resolves to some addresses whose connections get small
TCP segments (`m18-s3-aws-large-instances.md`).

The rows bound by the client's core grow with more clients. Separate
client processes side by side, each on its own core and sharing nothing:

| Request | Graviton4, 8 clients | AMD, 4 clients | AMD, 8 clients |
|---|---|---|---|
| HTTPS PUT | 34.5 to 36.2 GB/s | 24.0 GB/s | 16.7 GB/s, one run |
| GET | 26.1 to 26.4 GB/s | 22.0 to 22.1 GB/s | 24.8 to 26.1 GB/s |
| HTTPS GET | 17.6 to 17.9 GB/s | 14.7 to 14.8 GB/s | 21.6 to 22.1 GB/s |

Four clients fill the AMD instance's card over HTTPS, and eight were slower
there than four. These are processes started by hand, three runs per cell
unless marked. A client that runs several I/O threads itself is planned.

### Compared with other clients

s5cmd 2.3.0 and the AWS CLI 2.40.2 with its CRT transfer client, on the
Graviton4 instance against the same bucket, moving 8,000 objects of 8 MiB
held in RAM. Best run of each, with the cores busy across the machine.

| Request | s5cmd | AWS CLI with CRT | One client here | Eight clients here |
|---|---|---|---|---|
| PUT over HTTP | 22.1 GB/s on 72.0 cores | 6.5 on 5.3 | 36.9 on 3.9 | Not run: one fills the card |
| PUT over HTTPS | 14.3 on 54.0 | 6.6 on 7.5 | 5.3 on 1.3 | 36.2 on 13.5 |
| GET over HTTP | 11.0 on 42.4 | 4.0 on 5.6 | 4.4 on 1.2 | 26.4 on 10.9 |
| GET over HTTPS | 11.5 on 43.0 | 4.0 on 6.2 | 2.8 on 1.1 | 17.9 on 10.5 |

One client thread is the fastest of these for plain PUT and uses the fewest
cores per byte for every request. For HTTPS and for GET, s5cmd on 42 to 54
cores in its best runs moves more in total than one client thread does.
Four clients side by side pass it for HTTPS PUT and plain GET on both
machines and for HTTPS GET on the AMD machine. For HTTPS GET on the
Graviton4 machine four clients give 9.8 to 10.0 GB/s against s5cmd's 11.2
to 11.5, and eight clients give 17.6 to 17.9. Those tools read and write files, which costs them a copy
that our benchmark does not make, and their runs lasted 3 to 17 seconds.
The same file has the AMD figures and the conditions.

An earlier 32-core Graviton4 instance with a 100 Gbit/s card is in
`m14-s3-aws-us-east-1.md`. Plain PUT and PUT with checksums both filled
that card at 12.2 GB/s.

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

HTTPS speed can follow the processor's AES speed: in a profile of an HTTPS
upload on the DGX Spark, two thirds of the client's core is inside the
cipher. Sealing TLS records, one core, in GiB/s (`m13-aes-gcm-by-cpu.md`):

| Processor | GiB/s |
|---|---|
| AMD EPYC 9R05, Zen 5 (AWS `m8azn`) | 25.6 |
| AMD EPYC 9R45, Zen 5 (AWS `c8a`) | 22.2 |
| Intel Xeon 6975P-C (AWS `c8i`) | 16.2 |
| Arm Cortex-X925, faster routine (DGX Spark) | 12.2 |
| AMD EPYC 9R14, Zen 4 (AWS `c7a`) | 10.4 |
| Arm Cortex-X925, default build | 7.8 |
| AWS Graviton4 (AWS `c8g`) | 6.1 |

On the AMD `m8azn` instance HTTPS upload to S3 is 7.3 GB/s with large
segments and a 3 MiB send batch, and 3.5 GB/s through every address S3
returns with a 1 MiB batch, both with the client's thread full
(`m20-preference-and-comparison.md`). The profiles in
`m18-s3-aws-large-instances.md` show where the core goes in each case:
about half in AES-GCM with large segments, and mostly in the kernel's
transmit path with mixed addresses. HTTPS upload has not been measured
end to end on the Intel processor.

### Compared with the talk

The talk's figures are from its slides. Its hardware differs from ours and
the slides do not describe the method in detail, so read this as a rough
placement.

| Measurement | Talk | Here |
|---|---|---|
| PUT, plain HTTP | 25.2 GB/s per core | 36.7 GB/s to S3, with a 300 Gbit/s card full |
| PUT with CRC-64 checksums | 4.3 GB/s, the speed of its checksum | 16.4 GB/s to S3 |
| HTTPS PUT | No end-to-end figure. 7.4 GB/s for the cipher alone | 5.2 to 7.3 GB/s to S3, 7.6 GB/s between our machines |
| GET | 4.7 GB/s | 4.4 to 7.9 GB/s from S3, 6.4 GB/s between our machines |
| CRC-64 alone | 4.3 GB/s | 88 GB/s, and about 30 GB/s inside a PUT (`m5-checksums.md`) |
| AES-GCM alone | 7.4 GB/s | 6.5 to 27 GB/s, depending on the processor |

The talk reports "per core". Between our own machines plain PUT fills the
core the client runs on, and about 1.3 cores of work at 16 connections and
1.6 at 32 run on other cores, most of it in the kernel. To S3 at
36.7 GB/s the client's thread is half idle and about 3.4 more cores are
busy elsewhere on the machine. The slides do not say whether the
talk counts that work, so we cannot tell which of our numbers matches
theirs.

Request rates for small operations are in `m9-s3-ops-linode.md` and
`m14-s3-aws-us-east-1.md`. In both, the store's latency sets the rate and
the client is mostly idle.

## Limits

- Plain PUT fills a 200 Gbit/s link between our own machines and 100 and
  300 Gbit/s cards to S3, and reaches about 90 percent of the 200 Gbit/s
  card on the AMD instance. Where one thread
  stops is unknown. The S3 figures need the preference for large-segment
  addresses, which is off by default.
- With one client everything except plain PUT is bound by that client's
  core. With several clients the network card becomes the limit: eight
  Graviton4 HTTPS PUT clients, four AMD HTTPS PUT clients and eight AMD GET
  clients. Several clients have only been run as separate processes.
- On x86-64 the only real-store measurements are from one AMD instance
  type. Intel has not been measured against a store.
- On Linux 6.1 the client was run for minutes, not hours. Connections it
  had closed and replaced were still listed as established some seconds
  later there, and not on Linux 7.0. The cause was not looked for.
- TLS keys are never updated in place. A connection is retired after 2^23
  records and the next request handshakes again. If a server updates its
  keys in the middle of a connection, the request is retried on a new one.
- If a server sends extra bytes after a complete response and they arrive
  in a later read, the client does not notice. It does catch them when they
  arrive in the same read.
- One `store` or `fetch` moves at most one buffer. A larger object goes up
  as a multipart upload and comes down as ranged fetches, and the caller
  drives both.
- A listing with a delimiter pages by `start-after`, not by the store's
  continuation token. A truncated delimiter listing cannot yet be resumed
  reliably past a common prefix, because no `start-after` value skips every
  key under a prefix. Continuation tokens will fix this.
- A presigned URL does not check when temporary credentials expire. One
  signed with them stops working when they do, whatever its own expiry.
- A multipart upload that is neither completed nor aborted keeps its parts
  in the store. A bucket rule that expires incomplete uploads is the backstop.
- Azure Blob Storage cannot be used through the store yet, and its signing
  has not been tried against a real account.
- HTTP/1.1 only.
- Preferring large-segment addresses puts a store's connections on fewer
  of its addresses. The setting keeps a minimum number in use, and a store
  may still limit or shed load per address.
- Measuring a connection's segment size costs four system calls when the
  connection opens, and none after that.
- After connections to the preferred address family fail, the client stays
  on the other family until a connection to that one fails too. It does not
  try the preferred family again on its own.
- IPv6 has not been run against Amazon S3's dual-stack endpoints, only
  against Linode Object Storage and over loopback.
- Each measurement table states how many runs are behind its figures. The
  comparison with the talk takes its figures from those tables and from the
  files it names. Anything stated with no count is a single run.

## License

Apache License 2.0. See `LICENSE`.
