# Project Context

## Purpose
longshore is an object storage client in Rust built around Linux `io_uring`,
with a portable POSIX readiness backend (`kqueue` on macOS, `epoll` on Linux
hosts without `io_uring`). It moves large amounts of data with little CPU. It
talks to Amazon S3, S3-compatible stores and Google Cloud Storage. It is eight
small crates, and a ninth, `longshore`, that gathers them under one name.

## Tech Stack
- Rust, stable toolchain, edition 2021
- Linux 6.1 or later on x86-64 and AArch64 (`io_uring` through raw system
  calls), plus portable POSIX readiness (`kqueue` on macOS, `epoll` on Linux)
- AWS-LC (through `aws-lc-sys`) for AES-GCM, `rustls` for the TLS 1.3 handshake only
- `longshore_crc`'s own x86-64 and AArch64 kernels for CRC-64/NVME and CRC-32C
  (zero external dependencies; `crc-fast` is a dev/bench oracle only), `ring`
  for SHA-256 and HMAC

## Project Conventions

### First principles
Every change is checked against these. A feature that cannot meet them is
either redesigned or left out.

1. One thread drives all I/O from one completion queue (`io_uring` on Linux,
   or the portable `kqueue`/`epoll` engine elsewhere). No locks and no async
   runtime on the data path.
2. Memory is allocated when the client is built. Nothing allocates after
   that. The tests run under an allocator that aborts if something does. The
   TLS handshake is the one exception, because `rustls` allocates.
3. The low-level API and the high-level client's lending path never copy an
   object body. It goes from the caller's registered buffer to the socket,
   and from the socket into the caller's registered buffer. The high-level
   client's convenience forms, which take and return byte slices, copy the
   body once into or out of a registered buffer. That copy is by design and
   is documented on those forms.
4. Bodies are never run through a cryptographic hash. Requests are signed
   over the head only.
5. Protocol code does no I/O itself. It is written against the `Io` trait so
   the same code runs on the kernel and in the deterministic simulator.
6. Timers and refresh schedules are chosen by the client, not by servers.
7. A dependency on the data path is adopted only when a benchmark in
   `bench/results/` justifies it.

### Code Style
- `cargo fmt`, and `cargo clippy --workspace --all-targets -- -D warnings`
- Files stay under roughly 400 lines; split by responsibility before that
- Tests live in their own files, not inline with the code under test
- Every source file carries the Apache 2.0 header (`scripts/license-headers.py`)

### Architecture Patterns
- Nine workspace crates: `longshore_common` (`io`, `sim`), `longshore_crc`
  (`checksum`), `longshore_dns` (`dns`), `longshore_stream` (`stream`),
  `longshore_http_client` (`http`), `longshore_tls` (`tls`),
  `longshore_mailbox` (`mailbox`), `longshore_s3` (`store`, `crypto`,
  `client`), and the `longshore` facade crate that re-exports them (plus
  `longshore-bench` for benchmarks and real-kernel checks)
- Fixed-capacity slots for connections and operations, sized from `Limits`
  and `Config`
- Operations are submitted with a caller token and collected with `poll`

### Testing Strategy
- Simulation tests with seeded network fault injection (resets, short writes,
  partial reads, flipped bits, delays) for every protocol path
- Simulated disk fault and crash-recovery tests (`DiskFaults`, placed faults by
  submission number, `corrupt_medium_bit`, durable vs. unsynced writes and
  directories, whole-machine `power_loss` with sector-aligned torn writes, and
  zero-allocation WAL recovery under `alloc_guard`)
- Property tests with Hegel for parsers and codecs
- Fuzz targets for parsers that read bytes from the network
- Real-kernel self-checks on Linux for anything that touches `io_uring`
- A feature that talks to a store is also run against a real store before
  its tasks are marked done, and the result is recorded in `bench/results/`

### Git Workflow
- Feature branches and pull requests; nothing is pushed to `main` directly
- Every pull request goes through the no-mistakes gate

## Domain Context
- S3 request signing is SigV4; Google Cloud Storage uses a bearer token on its
  XML API
- Integrity is CRC-64/NVME on S3 and CRC-32C on Google Cloud Storage

## Important Constraints
- `io_uring` is Linux 6.1+ only; macOS (`kqueue`) and Linux without
  `io_uring` (`epoll`) run on the portable POSIX backend (without zero-copy
  sends or registered buffers, and macOS refuses core pinning). Other
  platforms build the crate and run the simulator, while building a client
  returns `BuildError::Unsupported`.
- Not planned: HTTP/2 (object stores gain nothing from it over pooled
  HTTP/1.1 connections and S3 does not offer it), interactive sign-in flows
  such as SSO, and Azure Blob Storage until an account is available to test
  against.

## External Dependencies
- Amazon S3, S3-compatible stores (tested on Linode Object Storage), Google
  Cloud Storage
- The EC2 instance metadata service and AWS STS, once credential providers land
