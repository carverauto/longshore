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
- Optional `tls` Cargo feature (on by default across `longshore_stream`,
  `longshore_http_client`, `longshore_s3`, and `longshore`): AWS-LC (through
  `aws-lc-sys`) for AES-GCM and `rustls` for the TLS 1.3 handshake only;
  `--no-default-features` excludes `longshore_tls`, `rustls`, and `aws-lc-sys`
- `longshore_crc`'s own x86-64 and AArch64 kernels for CRC-64/NVME and CRC-32C
  (zero external dependencies; `crc-fast` is a dev/bench oracle only), `ring`
  for SHA-256 and HMAC
- `loom` (`--cfg longshore_model`) for concurrency model checking of
  `longshore_mailbox` and `longshore_s3::client`

## Project Conventions

### First principles
Every change is checked against these. A feature that cannot meet them is
either redesigned or left out.

1. One thread drives all I/O from one completion queue (`io_uring` on Linux,
   or the portable `kqueue`/`epoll` engine elsewhere). No locks and no async
   runtime on the data path.
2. Memory is allocated when the backend or client is built (including fixed
   `FileSlot` tables, aligned/ring buffer pools, and preallocated mailbox
   waiters). Nothing allocates on the data path after setup. The tests run
   under an allocator (`alloc_guard`) that aborts if something does; the TLS
   handshake (`rustls`) is the sole exception, and mailbox waiter queues
   grow only if waiting callers exceed `Builder::waiters`.
3. The low-level API and the high-level client's lending path never copy a
   payload body: uploads, file writes/reads, and `ArmedRecv` completions
   transfer directly with caller-accessible registered buffers (with
   `Io::give_back` returning ring buffers without a syscall). The high-level
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
  `longshore_http_client` (`http`), `longshore_tls` (`tls`, behind the
  default-on `tls` Cargo feature), `longshore_mailbox` (`mailbox`),
  `longshore_s3` (`store`, `crypto`, `client`), and the `longshore` facade
  crate that re-exports them (plus `longshore-bench` for benchmarks and
  real-kernel checks)
- `Engine<W: World>` shares one `Io` contract state machine across
  `Virtual` (`SimIo`) and `Posix` (`PosixIo` via `kqueue` on macOS and
  `epoll` on Linux), alongside raw-syscall `UringIo` on Linux 6.1+
- Fixed-capacity slots for sockets, files (`FileSlot`, `sockets_max +
  files_max`), 4-class registered buffer pools (large, small, block-aligned
  for direct I/O registered in `<= 1 GiB` ranges, and `IORING_REGISTER_PBUF_RING`),
  and operations, sized from `Limits` (`Limits::BASE`) and `Config`
- Operations are submitted with a caller token (`Io::submit`, or linked
  file chains via `Io::submit_chain` / `IOSQE_IO_LINK`), support multishot
  `Op::ArmedAccept` and `Op::ArmedRecv` (`Completion::{more, buf}` +
  `Io::give_back`), `shutdown`-before-`close`, TCP options, and Unix-domain
  stream sockets; protocol state machines deliver outcomes via `poll`
- `longshore_mailbox` provides bounded lock-free `Copy` MPSC queues with
  `Io::waker()`/`Op::AwaitWake` wakeup, atomic `close` vs. `send`, `stop`
  draining, and generic `Pool`/`SlotWaiter`/`CallFuture`/`block_on` used by
  `longshore_s3::client`

### Testing Strategy
- Simulation tests with seeded network fault injection (resets, short writes,
  partial reads, flipped bits, delays) and deterministic trace-hash checks
  for every protocol path
- Simulated disk fault and crash-recovery tests (`DiskFaults`, placed faults by
  submission number, `corrupt_medium_bit`, durable vs. unsynced writes and
  directories, whole-machine `power_loss` with sector-aligned torn writes, and
  zero-allocation WAL recovery under `alloc_guard`)
- Shared generic `IO: Io` contract, file, chain, and server-networking tests
  across `SimIo`, `PosixIo` (`kqueue`/`epoll`), and `UringIo` (`contract_uring`,
  `file_uring`, `server_uring`), plus `fault-injection` stale-readiness and
  ring-failure checks (`client_faults`, `uring_faults`)
- Concurrency model checks via `scripts/test-model.sh` (`--cfg longshore_model`
  with `loom` for `longshore_mailbox` `mailbox_model` and `longshore_s3`
  `client_model`)
- Non-TLS verification (`--no-default-features` clippy/tests and `cargo tree`
  ensuring `longshore_tls` and `aws-lc-sys` are excluded) and opaque-box
  E2E tests (`crates/longshore/tests/e2e.rs`)
- Property tests with Hegel for parsers and codecs; fuzz targets in `fuzz/`
- Real-kernel self-checks on Linux (`scripts/test-linux.sh`, failing if no
  host is configured) and format/clippy/doc/`scripts/license-headers.py --check`
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
- `io_uring` is Linux 6.1+ only (using `<= 6.1` ring features plus
  `IORING_FEAT_NODROP`; `Builder::build` uses `UringIo` on Linux with no
  silent fallback unless `Builder::portable` selects `epoll`). `UringIo`
  requires sufficient `RLIMIT_MEMLOCK` (shared per user across all rings)
  and `RLIMIT_NOFILE >= sockets_max + files_max`. On 6.1, `FileSync`,
  `FileReserve`, `FilePunch`, `FileRename`, `FileUnlink`, `FileMkdir`, and
  creating/emptying `FileOpen` execute on kernel `io-wq` workers; truncate
  and directory listing are intentionally omitted.
- macOS (`kqueue`) and Linux `Builder::portable` (`epoll`) run on `PosixIo`
  (without zero-copy sends or registered buffers; macOS refuses core pinning
  with `BuildError::Setting` and runs file ops synchronously via `F_NOCACHE`,
  `F_FULLFSYNC`, best-effort `F_PREALLOCATE`, and `F_PUNCHHOLE`). Building
  with `tls(true)` when the `tls` feature is disabled returns
  `BuildError::Setting` naming `tls`. Other platforms build the crate and
  run the simulator, while `Builder::build` returns `BuildError::Unsupported`.
- Not planned: HTTP/2 (object stores gain nothing from it over pooled
  HTTP/1.1 connections and S3 does not offer it), interactive sign-in flows
  such as SSO, and Azure Blob Storage until an account is available to test
  against.

## External Dependencies
- Amazon S3, S3-compatible stores (tested on Linode Object Storage), Google
  Cloud Storage
- The EC2 instance metadata service and AWS STS, once credential providers land
