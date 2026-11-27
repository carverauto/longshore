# longshore_file

[![crates.io](https://img.shields.io/crates/v/longshore_file.svg)](https://crates.io/crates/longshore_file)
[![docs.rs](https://img.shields.io/docsrs/longshore_file)](https://docs.rs/longshore_file)
[![License](https://img.shields.io/crates/l/longshore_file.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

File I/O for programs that keep their own data on disk: a write-ahead
log, a store of blocks, an index. One API runs on `io_uring`, on the
portable `kqueue`/`epoll` backend, and in the simulator with its disk
faults and power loss.

`Files` is a fixed set of files and operations, driven from the caller's
own loop. It borrows the I/O backend on each call and never waits, so
files, TCP connections (`longshore_tcp`) and the object storage client
share one backend and one thread. Nothing allocates after the set is
built.

## What it gives

- Every operation returns a `Ticket` at once, or a `FileError` that says
  why it was refused; each accepted ticket reports exactly once, as an
  `Event`.
- The set resumes short reads and writes until they are whole.
- `Files::sequence` sends operations that must happen in order, such as a
  write and its sync, to the kernel as one chain.
- The set lends buffers from the backend's registered memory and never
  copies them.
- The set checks direct-I/O alignment before it submits anything, the same
  way on every backend.
- A `File` handle cannot reach a slot that has since been reused.
- Each open file keeps an operation of its own, so no file can take every
  slot.
- Every backend reports the same errors, a failed sync among them.

The crate is file I/O and nothing built on it. Logs, pages, records and
checksums are left to the program that uses it.

## Example

Open a file for direct I/O, then write one block and sync it as one
sequence:

```rust,no_run
use longshore_common::io::extents::Extents;
use longshore_common::io::posix::PosixIo;
use longshore_common::io::{Completions, Io, Limits, OpenFlags};
use longshore_file::{Event, FileConfig, FileSizes, Files, Step, Ticket};

// Four files with up to four operations each, over 16 aligned 1 MiB buffers.
let base = Limits::BASE
    .with_small_buffer_size(4096)
    .with_aligned_buffers(16)
    .with_aligned_buffer_size(1 << 20);
let (limits, config) = FileConfig::plan(base, FileSizes::new(4, 4)).unwrap();
let mut io = PosixIo::new(limits).unwrap();
let mut files = Files::new(config, &limits);
let mut x = Extents::new(limits.aligned(0), 16);
let mut out = Completions::new(limits.ops_max() as usize);

let flags = OpenFlags::NONE.with_write(true).with_create(true).with_direct(true);
let _: Ticket = files.open(&mut io, "/srv/app/log", flags).unwrap();
let file = loop {
    io.tick(true, &mut out).unwrap();
    while let Some(c) = out.pop() {
        let _mine: bool = files.on_completion(&mut io, &mut x, c);
    }
    files.poll(&mut io, &mut x);
    if let Some(Event::Done { file, result, .. }) = files.next_event() {
        let _: u64 = result.unwrap();
        break file.unwrap();
    }
};

// One block, written and then synced: `n` is 2. Two events follow, one per
// ticket; the second says whether the block is durable.
let extent = x.take().unwrap();
io.buf(extent.buf())[..4096].fill(0x42);
let steps = [
    Step::Write { file, extent, at: 0, len: 4096, file_off: 0 },
    Step::Sync { file, data_only: true },
];
let mut tickets = [Ticket::NONE; 2];
let n = files.sequence(&mut io, &mut x, &steps, &mut tickets).unwrap();
```

## Cost

File I/O through the set runs as fast as the raw `Op::File*` operations
beneath it. Direct I/O to a DGX Spark's NVMe, on the ring and on `epoll`,
separates the two paths in only 4 of 50 cells, and 3 of those favour the
set. On an Apple M3 Max with `kqueue`, reads cost the set 1 to 2 % more CPU
a GB, and writes show no difference.
[Measurements](https://github.com/carverauto/longshore/blob/main/docs/benchmarks.md#files-on-one-thread)
has the tables, and
[`m36`](https://github.com/carverauto/longshore/blob/main/bench/results/m36-file-bench-macos.md)
and
[`m37`](https://github.com/carverauto/longshore/blob/main/bench/results/m37-file-bench-dgx-spark-nvme.md)
the conditions.

## Platforms

On Linux 6.1, sync, reserve, punch, rename, unlink, mkdir, and an open
that creates or empties a file run on the kernel's `io-wq` workers.
Truncate and directory listing are left out: reopen a file with
`OpenFlags::with_empty(true)` to reset it, and keep an index file rather
than list a directory. macOS runs file operations synchronously, with
`F_NOCACHE` for direct I/O and `F_FULLFSYNC` for sync.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::file`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead. The crates are
released together: depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_file>
- Guide: [Files](https://github.com/carverauto/longshore/blob/main/docs/file.md):
  sizing, sharing a backend, durability, power loss
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
