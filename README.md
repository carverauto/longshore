<p align="center">
  <picture>
    <source
      media="(prefers-color-scheme: light)"
      srcset="assets/longshore-logo-light.png"
    />
    <img
      src="assets/longshore-logo.png"
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
lot of data with little CPU: one thread on Linux `io_uring`, all memory
allocated at start-up, and no copy of an upload's body made by the client,
which hands large sends to the kernel as zero-copy sends.

It talks to:

- Amazon S3
- S3-compatible stores, with the bucket in the host name or in the path. Run
  against Linode Object Storage and versitygw
- Google Cloud Storage, with a bearer token

The design follows the TigerBeetle talk
[The Fastest Object Storage Client in the World](https://youtu.be/NJrqmoNpVGg).

This is a library under development. A preview is published on crates.io as
`0.1.0-alpha.1`, and its API will change.
[Status](docs/status.md) lists what works and against which stores, and
[Limits](docs/limits.md) lists what has not been done, measured or proven.

## Performance

One client thread against Amazon S3, 8 MiB objects, from instances in the
same region as the bucket. Medians of five runs, and of eight for HTTPS PUT.
GB means 10^9 bytes.

| Request | Graviton4, 300 Gbit/s card | AMD Zen 5, 200 Gbit/s card |
|---|---|---|
| PUT, plain HTTP | 36.7 GB/s | 22.4 GB/s |
| PUT with a CRC-64 checksum on every body | 16.4 GB/s | 13.3 GB/s |
| GET | 4.4 GB/s | 7.9 GB/s |
| HTTPS PUT, 3 MiB send batch | 5.2 GB/s | 7.3 GB/s |
| HTTPS GET | 2.7 GB/s | 5.1 GB/s |

Plain PUT fills the 300 Gbit/s card with half of the client's thread idle.
Every row below it is bound by the one core the client runs on, and grows
with more I/O threads: eight of them store 31.1 GB/s over HTTPS on the
Graviton4 instance, with a checksum on every body.

These figures need the preference for large-segment addresses, which is off
by default. [Measurements](docs/benchmarks.md) has the conditions, the
figures for several threads, the comparison with s5cmd and the AWS CLI, and
runs between two machines. Every measurement is in
[`bench/results/`](bench/results/) with the conditions it was taken under.

## Using it

```toml
[dependencies]
longshore = "=0.1.0-alpha.1"
```

A preview has to be asked for by its exact version. The API documentation
is on [docs.rs](https://docs.rs/longshore), and for the `main` branch at
<https://carverauto.github.io/longshore/>.

`longshore::client` puts the store behind a handle that any thread can
share, over one or more I/O threads that share nothing.

```rust,no_run
use longshore::client::Builder;
use longshore::store::creds::Credentials;

let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
let mut settings = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials);
settings.tls = true;
settings.port = 443;
settings.threads = 4; // four independent I/O threads
let client = settings.build().unwrap();

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
threads. [The shareable client](docs/client.md) covers multipart uploads,
the calls that report exactly what the store answered, the settings, and
how it is tested. The store's own API is lower level:
[Building, testing and running against a store](docs/testing.md) shows how
to drive it.

## Requirements

- Linux 6.1 or later, on x86-64 or AArch64.
- A recent stable Rust toolchain.

The code builds on macOS and the simulator tests run there. Anything that
uses `io_uring` has to be cross-compiled and run on a Linux host.

```sh
cargo test --workspace          # unit, simulation and property tests
./scripts/test.sh               # those, the model checks, then the real-kernel checks
```

In a container, `io_uring` needs a seccomp profile that allows it and a
locked-memory limit large enough for the client's buffers. Both are set on
the node. See [Running in containers](docs/containers.md).

## Documentation

| Document | Contents |
|---|---|
| [Status](docs/status.md) | What works, and against which stores |
| [How it works](docs/design.md) | The design, and the crates in this repository |
| [The shareable client](docs/client.md) | The handle other threads use: operations, settings, failure behaviour |
| [Building, testing and running against a store](docs/testing.md) | The test scripts, and `s3_uring` against a real bucket |
| [Running in containers](docs/containers.md) | The seccomp profile and the locked-memory limit |
| [Measurements](docs/benchmarks.md) | Every benchmark table, with what bounds each figure |
| [Limits](docs/limits.md) | What has not been done, measured or proven |
| [`bench/README.md`](bench/README.md) | How to run the benchmarks |

## License

Apache License 2.0. See `LICENSE`.
