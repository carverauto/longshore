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
- The I/O layer it runs on comes with it: `longshore_file::io` (the
  backends among it) and the names the set's calls take (`Limits`,
  `OpenFlags`, `Extents`, `Completion`, `Completions`, `Io`), so a
  program needs no other crate.

The crate is file I/O and nothing built on it. Logs, pages, records and
checksums are left to the program that uses it.

## Example

Open a file for direct I/O, then write one block and sync it as one
sequence:

```rust,no_run
use longshore_file::io::posix::PosixIo;
use longshore_file::{
    Completions, Event, Extents, FileConfig, FileSizes, Files, Io, Limits, OpenFlags, Step, Ticket,
};

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

### From the network card to the disk

A server receives TCP data from the network and writes them to disk
with direct I/O. With `Rx::ZeroCopy`, the card puts each frame's payload
in pages that `longshore_tcp` lends, and those pages go to the disk as
they lie: no copy between the card and the disk. These runs were on a
pair of DGX Sparks (ConnectX-7, Linux 7.0, ext4 on a Samsung NVMe disk),
over one of the port's two interfaces (about 13.85 GB/s). The writes were
the raw `Op::File*` operations this crate is built on; the runs predate
the crate, and `m37` shows the set keeps pace with those operations.

**The receive.** `tcp_uring bulk`, MTU 8244, the sender's
frame-on-packet option on, three runs a cell (`m34`):

| Frames | Receiving cores | `ZeroCopy` GB/s | `ZeroCopy` CPU-s a GB | `Direct` GB/s | `Direct` CPU-s a GB |
|---|---|---|---|---|---|
| 64 KiB | 1 | 5.55 to 5.76 | 0.124 to 0.125 | 4.15 to 4.76 | 0.174 |
| 1 MiB | 1 | 2.20 to 6.57 | 0.076 to 0.093 | 2.19 to 3.10 | 0.149 to 0.156 |
| 64 KiB | 3 | 13.02 to 13.40 | 0.131 to 0.132 | 12.68 to 13.09 | 0.188 to 0.189 |
| 1 MiB | 3 | 10.16 to 12.05 | 0.086 to 0.090 | 9.21 to 11.29 | 0.165 to 0.168 |

Zero-copy kept 99.9 % of frames or more in their pages in every cell; in
direct mode the kernel copies every frame. Zero-copy receive carries
close to the interface's limit on three cores, at about 0.7 of direct
mode's CPU a GB with 64 KiB frames and about half with 1 MiB frames.
At this MTU, 1 MiB frames carried less than 64 KiB ones and varied more,
in every mode, with the receiving threads 20 to 69 % busy: the rates are
not the modes' capacity, and `m34` could not tell whether the sender
held them back (at MTU 9000, 1 MiB frames reach the interface's limit in
every mode). Compare the CPU. Frames between 64 KiB and 1 MiB were not
measured.

**The disk.** With eight 1 MiB direct-I/O writes in flight, `dd` alone
gets about 13 GB/s from the same disk until its fast cache fills: as much
as the interface carries.

**Card to disk.** `zc_uring log` receives one connection in
`Rx::ZeroCopy` on one thread and writes the pages where the card put
them. MTU 8244, one run each (`m31`):

| Pages a write | Writes in flight | GB/s | Receiving thread, CPU-s a GB |
|---|---|---|---|
| 256 (1 MiB) | 1 | 2.79 | 0.091 |
| 256 (1 MiB) | 4 | 4.12 | 0.092 |
| 1,024 (4 MiB) | 2 | 5.44 | 0.105 |

The writes set this rate, not the receive: 4 MiB writes, two in flight,
carry twice what 1 MiB writes do one at a time, while the thread's CPU a
GB barely moves.
A run with the receive on several cores and eight or more writes in
flight, which the card and the disk each allow at about 13 GB/s, has not
been made.

`tcp_uring path` is a log node: 64 connections, each 4 KiB body made
durable (`O_DSYNC`) and then sent on to three replicas. Three runs each:

| The body | Blocks a second | Node's CPU a block |
|---|---|---|
| Received at a block (`HeaderFraming::with_body_at_block`) | 10,813 to 11,295 | 10.2 to 10.6 µs |
| Moved to a block, then written | 10,237 to 11,816 | 10.9 to 11.2 µs |

One durable write a block sets the rate. A body that arrives already at a
block boundary costs the node about 5 % less CPU.
[`m34`](https://github.com/carverauto/longshore/blob/main/bench/results/m34-tcp-receive-modes-dgx-spark.md)
has every receive cell, and
[`m31`](https://github.com/carverauto/longshore/blob/main/bench/results/m31-tcp-zero-copy-receive-spike-dgx-spark.md)
the disk, the runs from card to disk and the log node.

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
