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

# longshore

longshore is an object storage client written in Rust. It is built to move a
lot of data with little CPU: one thread per core on Linux `io_uring`, all memory
allocated at start-up, and no copy of an upload's body made by the client,
which hands large sends to the kernel as zero-copy sends. On macOS (`kqueue`)
and on Linux hosts without `io_uring` (`epoll`), it also runs on a portable
POSIX readiness backend for development, tests, and non-ring environments.

It talks to:

- Amazon S3
- S3-compatible stores, with the bucket in the host name or in the path. Run
  against Linode Object Storage and versitygw
- Google Cloud Storage, with a bearer token

The design follows the TigerBeetle talk
[The Fastest Object Storage Client in the World](https://youtu.be/NJrqmoNpVGg).

This is a library under development. It is published on crates.io, and its
API will change between releases.
[Status][status] lists what works and against which stores, and
[Limits][limits] lists what has not been done, measured or proven.

## Performance

These figures are for the Linux `io_uring` backend (not the portable POSIX
backend). One client thread against Amazon S3, 8 MiB objects, from instances in
the same region as the bucket. Medians of five runs. GB means 10^9 bytes.

| Request | Graviton4, 300 Gbit/s card | AMD Zen 5, 200 Gbit/s card |
|---|---|---|
| PUT, plain HTTP | 36.4 GB/s | 23.9 GB/s |
| PUT with a CRC-64 checksum on every body | 16.7 GB/s | 14.1 GB/s |
| GET | 4.6 GB/s | 7.9 GB/s |
| HTTPS PUT | 5.1 GB/s | 7.6 GB/s |
| HTTPS GET | 2.8 GB/s | 5.2 GB/s |

Plain PUT fills the 300 Gbit/s card with half of the client's thread idle.
Every row below it is bound by the one core the client runs on, and grows
with more I/O threads: eight of them store 31.1 GB/s over HTTPS on the
Graviton4 instance, with a checksum on every body.

These figures need the preference for large-segment addresses, which is off
by default. [Measurements][benchmarks] has the conditions, the
figures for several threads, the comparison with s5cmd and the AWS CLI, and
runs between two machines. Every measurement is in
[`bench/results/`][results] with the conditions it was taken under.

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

Every operation returns a value that is a `Future` and also has a blocking
`wait`, so the same calls work under any async executor and from plain
threads. [The shareable client][client] covers multipart uploads,
the calls that report exactly what the store answered, the settings, and
how it is tested. The store's own API is lower level:
[Building, testing and running against a store][testing] shows how
to drive it.

## The I/O layer

The client is built on `longshore::io`, a single-threaded operation queue
that other storage code can use directly. It has direct file I/O with linked
chains, server-side networking with armed accept and receive over a buffer
ring, lock-free mailboxes between cores, and a simulated disk with power
loss and fault injection. TLS is a Cargo feature that is on by default.
[The I/O layer][io] describes each part.

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
[io]: https://github.com/carverauto/longshore/blob/main/docs/io.md
[limits]: https://github.com/carverauto/longshore/blob/main/docs/limits.md
[results]: https://github.com/carverauto/longshore/tree/main/bench/results
[status]: https://github.com/carverauto/longshore/blob/main/docs/status.md
[testing]: https://github.com/carverauto/longshore/blob/main/docs/testing.md
