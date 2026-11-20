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
longshore = "0.1"
```

The API documentation is on [docs.rs](https://docs.rs/longshore), and for
the `main` branch at <https://carverauto.github.io/longshore/>.

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
threads. [The shareable client][client] covers multipart uploads,
the calls that report exactly what the store answered, the settings, and
how it is tested. The store's own API is lower level:
[Building, testing and running against a store][testing] shows how
to drive it.

## File operations

`longshore::io` also supports local file I/O through fixed `FileSlot`s on the
same single-threaded `Io` queue (`UringIo`, `PosixIo`, and `SimIo`):

- **11 file operations**: `FileOpen` (with `read`, `write`, `create`,
  `exclusive`, `empty`, `direct`, and `directory` flags), `FileClose`,
  `FileRead`, `FileWrite` (64-bit offsets into registered buffers), `FileSync`
  (`datasync` or full sync), `FileReserve` (extend or keep size), `FilePunch`
  (block-aligned hole punching), `FileStatus` (256-byte Linux `statx` layout
  with `files::file_size`), `FileRename`, `FileUnlink`, and `FileMkdir`.
- **No truncate or directory listing**: reopen with `OpenFlags::empty` to reset
  a file from its start, and probe fixed names or keep an index file rather
  than listing a directory.
- **Linked chains (`Io::submit_chain`)**: submits file operations atomically
  (`IOSQE_IO_LINK` on `io_uring`), such as write-then-sync in one submission.
  A chain stops at the first failure or short read/write, completing later
  members with `-ECANCELED`.
- **Aligned pool**: set `Limits::aligned_buffers`,
  `Limits::aligned_buffer_size`, and `Limits::block_size` (a power of two
  `>= 4096` matching the storage device's sector/block size for `O_DIRECT` and
  `FilePunch`). Pools are registered with the kernel as `<= 1 GiB` contiguous
  ranges of whole buffers, removing the 16,384-buffer limit.

## Simulated disk

`longshore::sim::SimIo` includes a preallocated deterministic virtual disk for
testing file and WAL recovery alongside virtual sockets under one PRNG seed and
one virtual clock (zero heap allocation after setup):

- **Setup (`SimIo::setup_disk`)**: takes a `DiskConfig` fixing `max_files`,
  `max_dirs`, `max_bytes_per_file`, `max_name_len`, `write_log_entries`,
  `write_log_bytes`, `read_error_sectors`, and `sector_size` (`512` or `4096`).
- **Durable vs. unsynced state**: files hold durable bytes plus an ordered
  write log overlaid on `FileRead`. `FileSync` folds clean log entries into
  durable state; when the log is full, the oldest clean entry is written back
  automatically. Directories track durable names separately from unsynced
  create, mkdir, rename, and unlink changes until a directory `FileSync`, and
  open `FileSlot`s survive rename or unlink of their paths.
- **Whole-machine `SimIo::power_loss`**: drops every in-flight socket and file
  operation with no completion, frees all socket and file slots, clears timers
  and the waker flag, settles each unsynced write in order by the seed (whole,
  dropped, or cut at an interior sector boundary for multi-sector writes), and
  settles each unsynced directory change while preserving persistent sector
  read errors and the placed-fault operation counter.
- **Fault injection (`DiskFaults`, `PlacedFault`, `SimIo::corrupt_medium_bit`)**:
  `SimIo::set_disk_faults` sets 11 independent parts-per-million rates (all `0`
  by default, drawing zero PRNG values so traces without disk faults stay
  unchanged): `short_write` (aborts chains with `-ECANCELED`), `torn_reported`,
  `torn_silent`, `lost_reported`, `lost_silent`, `misdirected_write`,
  `read_error` (persistent per sector until covered by a full-sector write or
  `FilePunch`), `bit_flip` (read buffer only), `sync_fail` (`-EIO` with Linux
  `AS_EIO` write poisoning so covered writes never become durable),
  `reserve_refused` (`-ENOSPC`), and `punch_refused` (`-EOPNOTSUPP`).
  `SimIo::place_disk_fault` targets a 1-based submitted file operation with
  `Fail`, `FailAfter`, or any write fault, and `SimIo::disk_fault_counters`
  exposes zero-allocation per-fault counts.

## Serving with longshore

`longshore::io` and `longshore::stream` provide single-threaded server primitives
across `UringIo`, `PosixIo`, and `SimIo`:

- **TCP and Unix-domain listeners**: `Io::listen` binds a TCP listener, and
  `Io::listen_unix` binds a Unix-domain stream socket by path.
  `Op::ConnectUnix` copies a NUL-terminated path from a pool buffer at
  submission time into slot storage.
- **Armed accept (`Op::ArmedAccept`)**: accepts connections on a listener with
  one submission, placing each accepted socket into a free slot inside the
  reserved range `Limits::accept_range_start..accept_range_start +
  accept_range_len` and reporting the slot index with `Completion::more ==
  true`. When the range is full, the incoming connection is dropped and the
  armed accept ends with `-ENFILE` (or `-EMFILE`).
- **Socket options & shutdown-before-close**: `Io::no_delay` (`TCP_NODELAY`)
  and `Io::keep_alive` (`SO_KEEPALIVE`) apply to TCP listeners and
  descriptor-created TCP connects (failing the connect rather than falling back
  if options cannot be set; ignored for UDP and Unix-domain sockets).
  `Op::Close` cancels in-flight operations on the socket and submits `shutdown`
  before `close` on connected stream sockets so peers observe the end.
- **Byte stream (`longshore::stream::Stream`)**: plain-or-TLS stream wrapper
  over `Io` with zero HTTP dependency, usable on both connecting and accepting
  sides.

## Buffer ring

- **Configuration (`Limits`)**: set `Limits::ring_buffers` (up to `32_768`) and
  `Limits::ring_buffer_size` to add a fourth buffer class in the pool backed on
  Linux `io_uring` by page-aligned index memory (`IORING_REGISTER_PBUF_RING`).
  If `UringIo` cannot quiesce cleanly on drop, buffer-ring index memory is
  leaked alongside the pool.
- **Armed receive (`Op::ArmedRecv`)**: takes a buffer from the ring only when
  bytes arrive, so idle plain connections hold no buffer. Completions carrying
  bytes set `Completion::buf` to `Some(id)` and `Completion::more`; callers
  read bytes in place and return the buffer via `Io::give_back(id)` with no
  system call. Empty completions (`result <= 0`) return their buffer
  internally (`buf == None`), peer EOF ends with `0`, and an empty ring ends
  with `-ENOBUFS` leaving unread bytes in the socket. Armed operations count
  toward `Limits::ops_max` until their final (`more == false`) completion.

## TLS feature

- `tls` is a default-on Cargo feature across `longshore`, `longshore_s3`,
  `longshore_http_client`, and `longshore_stream`.
- Building `longshore` with `--no-default-features` excludes `longshore_tls`
  and `aws-lc-sys` from its resolved dependencies (`Builder::build` returns
  `BuildError::Setting` naming `tls` if TLS is requested).
- **What it does not remove**: `longshore_s3` still compiles C via `ring` for
  request signing, an active TLS `Stream` still keeps two TLS record buffers
  per connection, and default workspace test runs and `longshore-bench` still
  enable `tls` (non-TLS workspace tests run with `--exclude longshore-bench
  --no-default-features`).

## Passing messages between cores

- **`longshore::mailbox` (`longshore_mailbox`)**: bounded lock-free MPSC queue
  for `Copy` messages into a thread owning an `Io` (`Io::waker()` and
  `Op::AwaitWake`, with zero timers), plus a thread-parking receiver mode for
  threads without an `Io`.
- **Back pressure, `stop`, and atomic `close`**: `TrySendError::Full` returns
  the message without allocating; `stop` uses a `+ 1` reserved slot so prior
  messages are delivered first while later sends are refused; receiver `close`
  is atomic with `send`, keeping answers already written and answering
  sent-and-unanswered slots with the closed outcome.
- **Slot pools and futures**: generic `Pool` with a preallocated waiter queue
  (`Builder::waiters` on the S3 client; grows dynamically if exceeded),
  single- and multi-pool waiting (`SlotWaiter`), `detach` returning the
  caller's attached `u32` tag strictly after freeing the slot, `CallFuture`,
  and shared `block_on`.
- **Verification**: tested under `loom` (`scripts/test-model.sh`),
  `alloc_guard::sealed`, and deterministic multi-core `Engine<Virtual>`
  simulation.

## Requirements

- Linux 6.1 or later (x86-64 or AArch64) for `io_uring` (and `epoll` when
  `Builder::portable` is set), or macOS for the portable `kqueue` backend
  (without core pinning). Other targets build the crate and run the simulator,
  while `client::Builder::build` returns `BuildError::Unsupported`.
- A recent stable Rust toolchain.
- **`RLIMIT_MEMLOCK` and `RLIMIT_NOFILE`**: `UringIo` registers its buffer pool
  as locked memory (charged against `RLIMIT_MEMLOCK`, summed across all rings
  owned by the user; processes with `CAP_IPC_LOCK` are exempt) and sizes the
  fixed file table to `sockets_max + files_max` (which must not exceed
  `RLIMIT_NOFILE`).
- **Platform notes**:
  - On Linux 6.1 `io_uring`, `FileSync`, `FileReserve`, `FilePunch`,
    `FileRename`, `FileUnlink`, `FileMkdir`, and creating/emptying `FileOpen`
    execute on kernel `io-wq` worker threads and still report through the
    ring's completion queue.
  - On macOS (`PosixIo`), file operations block the calling thread:
    `OpenFlags::direct` uses `F_NOCACHE` (with alignment enforced by
    `io::files`), both `FileSync` modes use `F_FULLFSYNC`, `FileReserve` uses
    `F_PREALLOCATE` (which may set aside less space than requested), and
    `FilePunch` uses `F_PUNCHHOLE` (returning `-EOPNOTSUPP` on filesystems
    without support).

The simulator, unit, property, and portable `kqueue` backend and client tests
run directly on macOS. Anything that uses `io_uring` (and Linux `epoll`) has to
run on a Linux host.

```sh
cargo test --workspace                                                 # unit, simulation, property, portable and e2e tests
cargo test --workspace --exclude longshore-bench --no-default-features # plain build without longshore_tls / aws-lc-sys
cargo test -p longshore_common --features fault-injection              # stale-readiness & fault-injection tests
./scripts/test-model.sh                                                # loom model checks (longshore_mailbox + longshore_s3)
./scripts/test.sh                                                      # workspace tests, model checks, then real-kernel checks
cargo bench -p longshore-bench --bench <name>                          # criterion microbenchmarks (see bench/README.md)
```

To run the real-kernel checks (`scripts/test-linux.sh`) from a Mac or local
workstation, configure `~/.config/longshore/test-hosts` (or set
`LONGSHORE_TEST_HOSTS_FILE`) with one `<rust-target> <ssh-destination>` per
line, for example:

```text
aarch64-unknown-linux-musl user@arm-host
x86_64-unknown-linux-musl  user@x86-host
```

`scripts/test-linux.sh` fails if no host is configured so the `io_uring` checks
are never silently skipped.

In a container, `io_uring` needs a seccomp profile that allows it and a
locked-memory limit large enough for the client's buffers. Both are set on
the node. See [Running in containers][containers].

## Documentation

| Document | Contents |
|---|---|
| [Status][status] | What works, and against which stores |
| [How it works][design] | The design, and the crates in this repository |
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
[limits]: https://github.com/carverauto/longshore/blob/main/docs/limits.md
[results]: https://github.com/carverauto/longshore/tree/main/bench/results
[status]: https://github.com/carverauto/longshore/blob/main/docs/status.md
[testing]: https://github.com/carverauto/longshore/blob/main/docs/testing.md
