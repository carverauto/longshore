# The I/O layer

`longshore::io` is the single-threaded operation queue the client is built
on. It also does local file I/O, serves connections, and passes messages
between cores, and `longshore::sim` runs all of it against a simulated
network and disk. This page lists what each part offers.

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

## Resource limits and platform notes

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
