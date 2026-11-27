<p align="center">
  <picture>
    <source
      media="(prefers-color-scheme: light)"
      srcset="https://raw.githubusercontent.com/carverauto/longshore/v0.1.0/assets/longshore-logo-light.png"
    />
    <img
      src="https://raw.githubusercontent.com/carverauto/longshore/v0.1.0/assets/longshore-logo.png"
      alt="Longshore"
      width="900"
    />
  </picture>
</p>

<p align="center">
  <strong>The fastest object-storage client on the planet.</strong>
</p>

<p align="center">
  <a href="https://crates.io/crates/longshore"><img src="https://img.shields.io/crates/v/longshore.svg" alt="crates.io" /></a>
  <a href="https://docs.rs/longshore"><img src="https://img.shields.io/docsrs/longshore" alt="docs.rs" /></a>
  <a href="https://github.com/carverauto/longshore/blob/main/LICENSE"><img src="https://img.shields.io/crates/l/longshore.svg" alt="License" /></a>
</p>

# longshore

longshore is an object storage client in Rust that moves a lot of data with
little CPU. It runs one thread per core on Linux `io_uring`, allocates all its
memory at start-up, and never copies an upload's body: it hands large sends to
the kernel as zero-copy sends. The same code runs on a portable backend,
`kqueue` on macOS and `epoll` on Linux hosts without `io_uring`, for
development, tests and hosts without the ring.

Its object store talks to:

- Amazon S3
- S3-compatible stores, with the bucket in the host name or in the path. Run
  against Linode Object Storage and versitygw
- Google Cloud Storage, with a bearer token

On the same thread and backend, longshore also carries protocols that run
straight over TCP ([`longshore::tcp`](#tcp-connections)) and does file I/O
for programs that keep their own data on disk ([`longshore::file`](#files)).

The design follows the TigerBeetle talk
[The Fastest Object Storage Client in the World](https://youtu.be/NJrqmoNpVGg).

longshore is published on crates.io and still under development; its API
will change between releases. [Status][status] lists what works and against
which stores, and [Limits][limits] lists what has not been done, measured or
proven.

## Performance

These figures come from the Linux `io_uring` backend, not the portable one:
one client thread against Amazon S3, 8 MiB objects, instances in the bucket's
region, medians of five runs. GB means 10^9 bytes.

| Request | Graviton4, 300 Gbit/s card | AMD Zen 5, 200 Gbit/s card |
|---|---|---|
| PUT, plain HTTP | 36.4 GB/s | 23.9 GB/s |
| PUT with a CRC-64 checksum on every body | 16.7 GB/s | 14.1 GB/s |
| GET | 4.6 GB/s | 7.9 GB/s |
| HTTPS PUT | 5.1 GB/s | 7.6 GB/s |
| HTTPS GET | 2.8 GB/s | 5.2 GB/s |

Plain PUT fills the 300 Gbit/s card and leaves half the client's thread
idle. The client's one core bounds every row below it, and each grows with
more I/O threads: on the Graviton4 instance, eight threads store 31.1 GB/s
over HTTPS with a checksum on every body.

The figures need the preference for large-segment addresses, which is off
by default. [Measurements][benchmarks] has the conditions, the figures for
several threads, the comparison with s5cmd and the AWS CLI, and runs
between two machines. [`bench/results/`][results] holds every measurement
with its conditions.

## Using it

```toml
[dependencies]
longshore = "0.2"
```

The API documentation is on [docs.rs](https://docs.rs/longshore), and for
the `main` branch at <https://carverauto.github.io/longshore/>.

`longshore::client` puts the store behind a handle that any thread can
share, over one or more I/O threads that share nothing.

```rust,no_run
use longshore::client::Builder;
use longshore::store::creds::Credentials;

let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
let client = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials)
    .tls(true)
    .port(443)
    .threads(4) // four independent I/O threads
    .build()
    .unwrap();

// A body lives in a registered buffer borrowed from the client, so the
// client never copies it. The operation takes the buffer and hands it back.
let mut body = client.buffer_blocking();
body.as_mut_slice()[..5].copy_from_slice(b"hello");
let (stored, body) = client.store("greeting", body, 5).wait();
stored.unwrap();
let (fetched, body) = client.fetch("greeting", body).wait();
assert_eq!(fetched.unwrap(), Some(5));
assert_eq!(&body.as_slice()[..5], b"hello");
```

Every operation returns a `Future` that also has a blocking `wait`, so the
same calls work under any async executor and from plain threads.
[The shareable client][client] covers multipart uploads, the calls that
report exactly what the store answered, the settings, and how the client is
tested. The store's own API is lower level:
[Building, testing and running against a store][testing] shows how to
drive it.

## Crates

longshore is eleven crates. `longshore` gathers the other ten under one
name, and most programs depend on it alone. The crates are released
together: depend on all of them at the same version.

| Crate | In `longshore` as | What it does |
|---|---|---|
| [`longshore`](https://crates.io/crates/longshore) | | Re-exports every crate below; this README |
| [`longshore_s3`][c-s3] | `store`, `client`, `crypto` | The object store, and the client any thread can share |
| [`longshore_tcp`][c-tcp] | `tcp` | Connections and listeners for protocols over TCP: framing, frames lent in place |
| [`longshore_file`][c-file] | `file` | File I/O: a fixed set of files and operations, ordered sequences, direct I/O |
| [`longshore_common`][c-common] | `io`, `sim` | The `Io` trait, the `io_uring` and portable backends, and the simulator |
| [`longshore_http_client`][c-http] | `http` | HTTP/1.1: a connection pool, and an object server for tests |
| [`longshore_stream`][c-stream] | `stream` | One connection's byte stream, plain or TLS |
| [`longshore_tls`][c-tls] | `tls` | TLS 1.3: the record layer, and the handshake from rustls |
| [`longshore_mailbox`][c-mailbox] | `mailbox` | Lock-free mailboxes between threads and I/O cores |
| [`longshore_dns`][c-dns] | `dns` | DNS for a fixed set of names, read from a cache |
| [`longshore_crc`][c-crc] | `checksum` | CRC-64/NVME and CRC-32C |

`longshore-bench`, in the same workspace and not published, holds the
benchmarks and the real-kernel checks.

## The I/O layer

`longshore::io` is a single-threaded operation queue. The client is built
on it, and other storage code can use it directly. It gives direct file
I/O with linked chains, server-side networking with armed accept and
receive over a buffer ring, lock-free mailboxes between cores, and a
simulated disk with power loss and fault injection.
[The I/O layer][io] describes each part.

## TCP connections

`longshore::tcp` carries protocols that run straight over TCP or
Unix-domain sockets: replication, messaging, a database's own wire
protocol. The caller's loop drives a fixed set of connections and
listeners on the same backend and thread as file I/O and the object
storage client. Frames arrive whole and are lent where they were received:
a frame can be written to a direct-I/O log and sent on to other peers from
that same memory, so its bytes move once. TLS works with every provider,
and nothing allocates after the set is built.
[TCP connections][tcp] describes the API, the receive modes and sizing.

## Files

`longshore::file` does file I/O for programs that keep their own data on
disk. The caller's loop drives a fixed set of files and operations on the
same backend and thread as connections and the object storage client. The
set resumes reads and writes until they are whole, sends operations that
must happen in order to the kernel as one chain, and checks direct-I/O
alignment before it submits anything. Every backend reports the same
errors, a failed sync among them. Logs, pages, records and checksums are
left to the program that uses the crate.
[Files][file] describes the API, sizing and durability.

## TLS and crypto providers

HTTPS is TLS 1.3 with AES-GCM. rustls always runs the handshake. The crypto
provider, a Cargo feature, decides who seals and opens the records and
which crypto library rustls uses for the handshake.

| Provider | Feature flag | Records sealed and opened by | Handshake crypto | Use it when |
|---|---|---|---|---|
| AWS-LC | `aws-lc` (default; `tls` is the same) | AWS-LC, with no copy of the body | AWS-LC | Throughput matters most: the fastest |
| ring | `ring` | ring, copying each body once | ring | You want ring as the only crypto library, with no AWS-LC (and none of its C build) |
| rustls | `rustls` | rustls's own record layer | ring | You want all of TLS from rustls; it allocates a buffer for every record it sends |
| none | no provider feature | — | — | Plain HTTP, for networks where everything is internal |

### Performance

One client core, AES-128-GCM, 16 KiB records, in a Linux container on an
Apple M3 Max. GB = 10^9 bytes. HTTPS runs over loopback with 8 connections
and 8 MiB bodies; CPU is the client thread's per GB moved.

| Provider | Seal | Open | HTTPS PUT | CPU per GB | HTTPS GET | CPU per GB |
|---|---|---|---|---|---|---|
| none (plain HTTP) | — | — | 12.1 GB/s | 82 ms | 4.08 GB/s | 245 ms |
| AWS-LC | 10.2 GB/s | 11.1 GB/s | 7.20 GB/s | 139 ms | 2.99 GB/s | 333 ms |
| ring | 6.8 GB/s | 6.6 GB/s | 5.14 GB/s | 195 ms | 2.59 GB/s | 385 ms |
| rustls | 6.2 GB/s | 6.7 GB/s | 4.61 GB/s | 217 ms | 2.49 GB/s | 402 ms |

Seal and open measure the record layer alone (`cargo bench -p
longshore-bench --bench tls_providers`); PUT and GET come from `http_uring
--provider`. With ring, TLS adds about twice as much CPU to an upload as
with AWS-LC. [`bench/results/m29-tls-providers.md`][m29] has the
conditions and ranges; x86-64 and cloud hosts are still to be measured.

### Switching a provider on

AWS-LC, the default:

```toml
[dependencies]
longshore = "0.2"
```

ring only, with no AWS-LC in the build:

```toml
[dependencies]
longshore = { version = "0.2", default-features = false, features = ["ring"] }
```

rustls for everything:

```toml
[dependencies]
longshore = { version = "0.2", default-features = false, features = ["rustls"] }
```

No TLS at all (plain HTTP; `ring` stays in the build for request signing):

```toml
[dependencies]
longshore = { version = "0.2", default-features = false }
```

Then turn HTTPS on in the client with `.tls(true)`, as in [Using it](#using-it).
Asking for TLS in a build without a provider makes `build()` fail with an
error that names the feature to enable.

### Choosing at run time

Features add up, so a build can hold several providers, for example
`features = ["aws-lc", "ring"]`, or because two dependencies ask for
different ones. The client then uses the first compiled in of AWS-LC, ring
and rustls, unless told otherwise:

```rust,no_run
use longshore::client::{Builder, TlsProvider};
use longshore::store::creds::Credentials;

let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
let client = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials)
    .tls(true)
    .port(443)
    .tls_provider(TlsProvider::Ring)
    .build()
    .unwrap();
```

Choosing a provider that is not compiled in makes `build()` fail with an
error that names its feature. `TlsProvider::compiled()` says which are
available.

## Requirements

- Linux 6.1 or later (x86-64 or AArch64) for `io_uring` (and `epoll` when
  `Builder::portable(true)` is set), or macOS for the portable `kqueue` backend
  (without core pinning). Other targets build the crate and run the simulator,
  while `client::Builder::build` returns `BuildError::Unsupported`.
- A recent stable Rust toolchain.

[Building, testing and running against a store][testing] has the test
commands and how to run the real-kernel checks on a Linux host.
[The I/O layer][io] lists the resource limits and the platform notes.

In a container, `io_uring` needs a seccomp profile that allows it and a
locked-memory limit large enough for the client's buffers. Both are set on
the node. See [Running in containers][containers].

## Documentation

| Document | Contents |
|---|---|
| [Status][status] | What works, and against which stores |
| [How it works][design] | The design, and the crates in this repository |
| [The I/O layer][io] | File operations, serving, the buffer ring, mailboxes, the simulated disk |
| [TCP connections][tcp] | `longshore::tcp`: connections, framing, frame memory, TLS, sizing |
| [Files][file] | `longshore::file`: files, sequences, direct I/O, durability, sizing |
| [The shareable client][client] | The handle other threads use: operations, settings, failure behaviour |
| [Building, testing and running against a store][testing] | The test scripts, and `s3_uring` against a real bucket |
| [Running in containers][containers] | The seccomp profile and the locked-memory limit |
| [Measurements][benchmarks] | Every benchmark table, with what bounds each figure |
| [Limits][limits] | What has not been done, measured or proven |
| [`bench/README.md`][bench] | How to run the benchmarks |

## License

Apache License 2.0. See `LICENSE`.

[bench]: https://github.com/carverauto/longshore/blob/main/bench/README.md
[benchmarks]: https://github.com/carverauto/longshore/blob/main/docs/benchmarks.md
[client]: https://github.com/carverauto/longshore/blob/main/docs/client.md
[containers]: https://github.com/carverauto/longshore/blob/main/docs/containers.md
[design]: https://github.com/carverauto/longshore/blob/main/docs/design.md
[file]: https://github.com/carverauto/longshore/blob/main/docs/file.md
[io]: https://github.com/carverauto/longshore/blob/main/docs/io.md
[tcp]: https://github.com/carverauto/longshore/blob/main/docs/tcp.md
[limits]: https://github.com/carverauto/longshore/blob/main/docs/limits.md
[m29]: https://github.com/carverauto/longshore/blob/main/bench/results/m29-tls-providers.md
[c-common]: https://github.com/carverauto/longshore/blob/main/crates/longshore_common/README.md
[c-crc]: https://github.com/carverauto/longshore/blob/main/crates/longshore_crc/README.md
[c-dns]: https://github.com/carverauto/longshore/blob/main/crates/longshore_dns/README.md
[c-file]: https://github.com/carverauto/longshore/blob/main/crates/longshore_file/README.md
[c-http]: https://github.com/carverauto/longshore/blob/main/crates/longshore_http_client/README.md
[c-mailbox]: https://github.com/carverauto/longshore/blob/main/crates/longshore_mailbox/README.md
[c-s3]: https://github.com/carverauto/longshore/blob/main/crates/longshore_s3/README.md
[c-stream]: https://github.com/carverauto/longshore/blob/main/crates/longshore_stream/README.md
[c-tcp]: https://github.com/carverauto/longshore/blob/main/crates/longshore_tcp/README.md
[c-tls]: https://github.com/carverauto/longshore/blob/main/crates/longshore_tls/README.md
[results]: https://github.com/carverauto/longshore/tree/main/bench/results
[status]: https://github.com/carverauto/longshore/blob/main/docs/status.md
[testing]: https://github.com/carverauto/longshore/blob/main/docs/testing.md
