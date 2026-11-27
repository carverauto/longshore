# Files

`longshore::file` (the crate `longshore_file`) is file I/O for programs
that keep their own data on disk: a write-ahead log, a store of blocks, an
index. It gives a fixed set of files and operations, whole transfers,
ordered sequences, direct I/O checked the same way on every backend, and
one error type. This page describes the API, sizing, durability and what
the crate does not do.

## What it is

`Files` is the set. Like `longshore::tcp`'s `Net`, it is driven from the
caller's own loop on the caller's own thread: it borrows the I/O backend
on each call, never waits, never reads a clock and never collects
completions itself. It submits in three places only: the call that asks
for an operation, `on_completion`, and `poll`. Files, connections and the
object storage client can therefore share one backend and one thread.

The set is written against the `Io` trait, so the same program runs on
`UringIo`, on `PosixIo` (`kqueue` on macOS, `epoll` on Linux) and in the
simulator. Below it are `longshore_common`'s file operations
(`Op::FileOpen` to `Op::FileMkdir`, `Op::FileWriteSpans`), which stay
available to a program that wants them raw: the set picks file slots,
tokens and buffers, routes completions back, resumes short transfers,
checks direct-I/O alignment and orders operations, which a program using
the raw operations does itself.

Everything is allocated when the set is built. After that nothing
allocates, `power_lost` included.

## What it is not

The crate is file I/O and nothing built on it. It has no log, segment,
page, record or cache, and it computes no checksum of the bytes it moves.
A write-ahead log or a storage engine is built on it by its user, who
decides the format, what a record is, how it is checked, and how it is
recovered. Truncate and directory listing are left out, as they are of the
ring on Linux 6.1 (`openspec/project.md`): reopen a file with
`OpenFlags::with_empty(true)` to reset it, and keep an index file rather
than list a directory. There is no timer and no cancel: an operation the
kernel never completes keeps its slot, and its file's close waits for it
for ever.

## One turn

```text
FileConfig::plan(limits, sizes)           once: the Limits with the set's share added
Files::new(config, &limits)               once: every table of the set
files.open(&mut io, path, flags)          a Ticket at once, or a refusal
io.tick(wait, &mut out)                   collect
files.on_completion(&mut io, &mut x, c)   route: false means "not mine"
files.poll(&mut io, &mut x)               submit what waited for room
files.next_event()                        pull events until None
```

Every operation returns a `Ticket` at once, or a `FileError` that says why
it was refused; a refusal submits nothing and takes nothing. The outcome
comes later as one `Event::Done { ticket, file, result }`: `result` is the
bytes transferred, the size for `status`, 0 for the rest, or the error.
`file` is the new handle for an open, the file for a file operation and
`None` for a path operation. Every accepted ticket reports exactly once,
unless `power_lost` ended it.
Each accepted operation takes a record: an operation slot, or for an open
or a close the file slot's own record. The event queue holds one event a
record, `ops + files` in all, and a record is free again only once its
event has been taken, so the queue cannot overflow: not pulling events is
the backpressure.

`x` is the caller's `Extents` over the block-aligned class of the pool
(see "Memory"). It is passed to every call that names a buffer, and to
`on_completion`, `poll` and `power_lost`.

### Sharing the backend

Plan each component on top of the `Limits` the one before returned, give
each its own range of tokens, and build the backend once from the last
`Limits`:

```rust
use longshore::file::{FileConfig, FileSizes};
use longshore::tcp::{Config, Sizes};

// `base`: the Limits of whatever was planned before.
let (limits, net) = Config::plan(base, Sizes::new(4, 64, 1)).unwrap();
let after_net = net.token_base() + net.tokens();
let sizes = FileSizes::new(8, 4).with_token_base(after_net);
let (limits, files) = FileConfig::plan(limits, sizes).unwrap();
```

A completion with a token of the set's range (`FileConfig::token_base`,
`FileConfig::tokens`) is the set's. With `longshore::io::turn::Turn`, give
the set's range as storage and the connections' as network: the set's
completions of a batch are then handed out before the connections'. That
orders a batch; it does not batch commits, which is the program's to do.

## An example

Four files with up to four operations each, a write and its sync as one
sequence, and the loop that drives it:

```rust
use longshore::file::{Event, FileConfig, FileSizes, Files, Step, Ticket};
use longshore::io::extents::Extents;
use longshore::io::posix::PosixIo;
use longshore::io::{Completions, Io, Limits, OpenFlags};

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
        let _: bool = files.on_completion(&mut io, &mut x, c);
    }
    files.poll(&mut io, &mut x);
    if let Some(Event::Done { file, result, .. }) = files.next_event() {
        let _: u64 = result.unwrap();
        break file.unwrap();
    }
};

// One block, written and then synced; the set holds the extent until the
// write has reported, the caller keeps its own hold.
let extent = x.take().unwrap();
io.buf(extent.buf())[..4096].fill(0x42);
let steps = [
    Step::Write { file, extent, at: 0, len: 4096, file_off: 0 },
    Step::Sync { file, data_only: true },
];
let mut tickets = [Ticket::NONE; 2];
let n = files.sequence(&mut io, &mut x, &steps, &mut tickets).unwrap();
```

`n` is 2. The two events that follow name `tickets[0]` and `tickets[1]`;
the second says whether the block is durable. `Ticket::NONE` only fills
the slice: no operation is given it and no event names it.

## Sizing

`FileConfig::plan(limits, sizes)` adds what the set needs to `limits` and
returns both: build the backend from the `Limits` and the set from the
`FileConfig`.

| `FileSizes` | Meaning | Default |
|---|---|---|
| `new(files, ops_per_file)` | Files open at once, and operations at once a file | none |
| `with_shared(n)` | Operation slots shared by every file, beyond each file's own | `files` |
| `with_paths(n)` | Path operations at once (`status`, `rename`, `unlink`, `mkdir`), each with a slot and a path buffer of its own | 4 |
| `with_sequence_max(n)` | Steps of one ordered sequence, at most `ops_per_file` | `ops_per_file` |
| `with_spans_max(n)` | Spans of one gathered write, 1 to 1,024 | 64 |
| `with_token_base(t)` | The set's first token; it uses `FileConfig::tokens()` of them | 0 |

What the plan adds, with `ops = files + paths + shared`:

| Count | Added |
|---|---|
| `ops_max` | `ops` |
| `files_max` | `files` |
| `gather_ops`, `gather_spans` | `ops` rows; spans the larger of what was there and `spans_max` |
| small buffers | `files` (a path for each file's open), `paths` (the paths and status area of each path operation), `ops` (a list of spans for each operation slot) |
| tokens | `FileConfig::tokens()` from `token_base`, one a record: `ops` operation records, and one a file slot for its open and close, `ops + files` in all |

The plan does not add block-aligned buffers: those are the caller's,
sized with `Limits::with_aligned_buffers` and lent through `Extents`, so
that a buffer received from a connection and a buffer written to a file
can be the same buffer.

The small class's size is the pool's, shared with other components. The
plan refuses one that cannot hold a list of `spans_max` spans (twelve
bytes each), or a 256-byte status area and a path. A path holds
`FileConfig::path_max()` bytes, `(small buffer size − 256) / 2 − 1`, so
that the two paths of a rename and the status area fit one buffer: 1,919
bytes with buffers of 4 KiB.

**No file can take every slot.** Each file has an operation slot of its
own, reserved when the set is built. An open file uses its own slot
first, then shared slots, up to `ops_per_file` at once; a path operation
uses only the path slots; a file's open uses its own path buffer and its
own slot. So every open file can always start one operation, path
operations never take a file's share, and one busy file holds at most
`ops_per_file − 1` shared slots. Gather rows and span lists are one an
operation slot, so no file's gathered writes can use up another's. An
operation beyond a file's share, or with no shared slot left, is refused
`Full`.

The plan refuses `ops_per_file` of 0 or above `shared + 1`, or `files` of
0 (`ConfigError::Ops`); `sequence_max` of 0 or above `ops_per_file`
(`Sequence`); `spans_max` of 0 or above 1,024 (`Spans`); a small class too
small (`ClassSize`); and tokens or counts that overflow (`Tokens`).

## Handles

A `File` is a file slot and a generation. Closing a file raises its
slot's generation, so a handle kept past its close is refused `Stale` and
never reaches the file opened in the slot next; generations wrap at
2^32. A file slot is in one of five states:

| State | Accepts | Left by |
|---|---|---|
| Free | `open`, once the event of its last close or failed open has been taken | an open accepted → Opening |
| Opening | nothing | the open's completion: success → Open; failure → Free |
| Open | every operation | `close` → Closing; a failed sync, or a write of a file opened with `with_dsync` that fails with `EIO`, `ENOSPC` or `EDQUOT` → Poisoned |
| Poisoned | `read_at` and `close`; the rest are refused `Poisoned` | `close` → Closing |
| Closing | nothing (`Closing`) | the close's completion → Free |

A close is held back until every other operation of the file has
reported, and is then submitted in the room of the file's own slot at the
backend, so `close` is never `Full`. Opens and closes have records of
their own, one a file slot, so a file's own slot still holding an event
not yet taken never makes them wait. Operations accepted before the close
still run. A close that fails is reported in its event, and the slot is
freed all the same.

## Operations

| Operation | Needs | The set holds while it is in flight | `u64` of the result |
|---|---|---|---|
| `open(path, flags)` | a free file slot | its path buffer | 0; the `File` is in `file` |
| `close(file)` | – | – | 0 |
| `read_at(file, x, (extent, at, len), file_off)` | not opened write-only | the extent | bytes read; fewer only at the end of the file |
| `write_at(file, x, (extent, at, len), file_off)` | opened for writing | the extent | bytes written: all, or an error |
| `write_spans(file, x, &[Span], file_off)` | opened for writing | each extent span; the slot's span list | as `write_at` |
| `sync(file, data_only)` | – | – | 0 |
| `reserve(file, (off, len), keep_size)` | opened for writing, `len > 0` | – | 0; best effort on macOS |
| `punch(file, (off, len))` | opened for writing, `len > 0`, whole blocks | – | 0 |
| `status(path)` | a path slot | its path buffer and status area | the size |
| `rename(from, to)`, `unlink(path)`, `mkdir(path)` | a path slot | its path buffer | 0 |

`OpenFlags` sets read, write, create, exclusive, empty (truncate at open),
direct I/O, directory and durable writes (`with_dsync`). A directory is a
file: open it with `with_directory(true)` and `sync` it to make a create,
a rename or an unlink in it durable.

Paths are copied into the set's path buffers when the operation is asked
for; a path is not payload. One longer than `path_max` or with a NUL
inside is refused `BadPath`.

A gathered write takes up to `spans_max` spans, each `Span::Extent {
extent, at, len }`, held by the set, or `Span::Page { page, off, len }`,
a page of the receive area (any other buffer is refused `TooLarge`) that
the caller holds until the write's event
(`longshore::tcp`'s `WriteHold`). A frame received into pages
(`Rx::ZeroCopy`) is so written to a file without a copy.

Operations that are not in a sequence run in any order, even of one file.
A sync covers the writes that completed before it was asked for; a write
still in flight when the sync is asked for may or may not be covered.

## Whole transfers

A read or write that the backend completes short is resumed for the rest
from `on_completion`, under the same ticket, until it is whole, the file
ends (a read) or an error ends it. The event says how much was moved.

- Every resume moves at least one byte. A write that completes with 0
  bytes ends `NoProgress`; a read that completes with 0 before the end it
  asked for has reached the end of the file, and reports what it read.
- On a direct-I/O file, a short write resumes from what was written
  rounded down to a block, rewriting the same bytes from the same buffer.
  One that wrote less than a block is written again from the same offset,
  at most four times in a row: a fifth write in a row with no whole block
  ends `NoProgress`, so resumes are bounded. A short read that ends off a
  block boundary is the end of the file.
- An error after some bytes were moved ends the transfer
  `Partial { done, kind }`; with none moved, the plain error. A failed
  write of a file opened with `with_dsync` that lost its bytes (see
  "What a failed sync means") ends `Partial { done, kind: SyncFailed }`
  when bytes were done before it.

A program that wants the backend's short completions as they are uses
`longshore_common`'s operations directly.

## Ordered sequences

`sequence(io, x, &steps, &mut tickets)` takes up to `sequence_max` steps,
each any operation but open and close, of any file or a path, and writes
their tickets into `tickets` in order. Each step counts against its own
file's share or the path slots, and the whole sequence is refused `Full`
unless every step has room; every step is checked before anything is
submitted, as a single call would be, and a refused sequence leaves the
tickets of the steps before the refused one `Ticket::NONE`. The steps go
to the backend as one chain (`IOSQE_IO_LINK` on the ring; the portable
backend and the simulator order them themselves).

- Each step starts after the one before it is whole. When the backend
  ends a chain early because a transfer was short, the set waits for
  every member to report, resumes the short one, and submits it and the
  rest as a new chain.
- When a step fails, the steps after it are not performed and report
  `Cancelled`. Every step reports.
- The checks leave the backend nothing to refuse of the steps themselves.
  Should it refuse the chain all the same for want of its own room
  (another component took room the plan gave the set), the refused step
  reports the backend's errno as its plain error (`Os` for a resource the
  backend had none of, `NoSpace`, `Invalid` and the rest as they map),
  the others `Cancelled`, and no file is poisoned by it.
- A sequence waiting for room is not overtaken: waiting work is submitted
  first in, first out.

**Replacing a file durably.** Write a temporary file, sync it, rename it
over the old name, and sync the directory. With the temporary file and
the directory open, that is one sequence:

```rust
let steps = [
    Step::Write { file: tmp, extent, at: 0, len, file_off: 0 },
    Step::Sync { file: tmp, data_only: true },
    Step::Rename { from: "/srv/app/index.tmp", to: "/srv/app/index" },
    Step::Sync { file: dir, data_only: false },
];
```

A data sync is enough for the temporary file: it includes the metadata
needed to read the data back, the size among it. Because the temporary
file is durable before the rename, a power loss at any point leaves the
old file or the new one under the name, each whole; once the directory's
sync has reported success, it is the new one. If the write or the first
sync fails, the rename is cancelled and the name keeps the old file. The
plan needs `sequence_max` of 4 at least, `ops_per_file` as large, and a
path slot.

## Memory

Buffers are the pool's block-aligned class, taken from the caller's
`Extents`. The set adds a hold on every extent an operation names when the
operation is accepted, and releases it when the operation reports, so a
caller can release its own hold as soon as it has asked for the write: the
buffer is not given out again until the write is done. A hold that cannot
be taken refuses the call (`StaleExtent` for an extent freed since it was
taken, `TooManyHolds`). The same buffer can be sent on a connection and
written to a file at once, each holding it until done. No payload is
copied.

A release of one of the set's own holds can fail only if the caller
released a hold that was the set's. That is a broken invariant: it is
asserted in debug builds and counted in `FileUsage::broken_holds`.

## Direct I/O

A file opened with `with_direct(true)` is read and written past the page
cache (`O_DIRECT` on Linux, `F_NOCACHE` on macOS). Before anything is
submitted the set checks every transfer of such a file against the
block size of the pool (`Limits::with_block_size`): each extent span is of
the block-aligned class and starts at a block boundary, and every length
and file offset is whole blocks. Anything else is refused `Unaligned`.
The check is the set's, so a misaligned transfer is the same refusal on
every backend, including macOS, whose kernel would take it, and the
simulator.

On every file, `punch` takes whole blocks, `reserve` and `punch` take a
length above 0, a range must lie inside its extent, and a transfer
(`read_at`, `write_at`, or a gathered write's spans together) moves at
most `i32::MAX` bytes, since every backend counts a completion's bytes in
an `i32` (`TooLarge`, as a step of a sequence too).

A file system without direct I/O refuses the open with `EINVAL`, and the
event carries `Invalid { errno }`.

## Durability

A write that has completed is in the kernel's cache, not yet on stable
storage. It is durable after a sync of its file that was asked for after
the write completed and that reported success: on Linux `sync(file,
true)` syncs the data and the metadata needed to read it back
(`fdatasync`), `sync(file, false)` all of the file's metadata as well
(`fsync`). On macOS every sync is `F_FULLFSYNC`, whatever `data_only`
says, which also flushes the drive's cache. A file
opened with `with_dsync` makes each write durable when it completes, with
no sync. A new file's name, or a renamed one, is durable once its
directory has been synced after the create or rename.

On macOS a durable write (`with_dsync`) reaches the drive but not past
its cache; only a sync flushes that.

### What a failed sync means

A failed sync poisons the file. Every failure of a sync is
`SyncFailed { errno }`, whatever its errno, except `Unsupported`: a failed
write-back can come as `EIO`, as `ENOSPC` or as another errno, and the
portable backend reports what it does not know as `EIO`. A write of a
file opened with `with_dsync` that fails with `EIO`, `ENOSPC` or `EDQUOT`
is a failed sync too, a durable write lost: it reports `SyncFailed` (or
`Partial { done, kind: SyncFailed }` when bytes were done before it) and
poisons the same way. Other errors of such a write are reported as they
are and do not poison.

The reason is the kernel's. When write-back of a dirty page fails, Linux
reports the error to the next sync and may then drop the page or mark it
clean; a later sync can succeed though the bytes never reached the disk.
So once a sync has failed, the bytes written since the last good sync are
of unknown state, and no later sync makes them good.

A Poisoned file accepts reads and its close, and refuses everything else
`Poisoned`. Its operations waiting for room end `Poisoned` at once; a
step of a sequence waiting for room ends so when its chain comes up for
submission. Its transfers being resumed end `Partial { done, kind:
Poisoned }`, and its operations already in flight report their own
outcomes. Reads stay
allowed because the bytes may still be readable; only their durability is
lost. The program decides what follows: close the file and recover from
what it knows was durable, rewrite the bytes from its own copy into a new
file, or stop.

## Power loss

On the simulator, `SimIo::power_loss` drops every operation in flight
without a completion. `Files::power_lost(&mut x)` is the set's half of it:
it frees every file and operation slot, raises every generation, drops
waiting work and the events not yet taken, releases every hold, and
allocates nothing. The tickets
in flight end without events and every handle is stale. The program then
opens its files again and recovers.

After a power loss, every byte is there that was written by a write that
completed whole, followed by a sync of that file asked for after the write
completed and reported success, with no failed sync of the file in
between, and with the file's name made durable by a sync of its
directory. Nothing else is promised: a write not followed by such a sync
may be there, absent, or torn at a sector boundary.

## What is not detected

The set reports what the backend reports. A write that the drive or the
kernel loses, tears or puts in the wrong place and reports done, and a bit
that flips on the medium, are not detected: that takes a checksum of the
bytes, which the crate leaves to its users. A log checks its records with
a checksum of its own (`longshore::checksum` has CRC-32C and CRC-64/NVME)
and recovers to the last record that checks. The simulator injects every
such fault (`DiskFaults`), so such a recovery can be tested.

## Errors

`FileError` is one type, the same on every backend, and flat. Refused at
the call, with nothing submitted:

| Error | Meaning |
|---|---|
| `Full` | No operation slot for this file or in the set, or no path slot |
| `Stale` | The handle's file was closed |
| `Closing` | The file is closing |
| `Poisoned` | A sync of the file failed, or a durable write was lost: only reads and `close` |
| `NotOpen` | Not opened for this: a write of a read-only file, a read of a write-only one |
| `Unaligned` | On a direct-I/O file, a buffer, length or offset that is not whole blocks |
| `TooLarge` | More spans or steps than planned, a range past its extent, a transfer of more than `i32::MAX` bytes, a length of 0 where one is needed |
| `BadPath` | A path longer than `path_max`, or with a NUL inside |
| `StaleExtent`, `TooManyHolds` | An extent that cannot be held |

Reported in an event:

| Error | Meaning |
|---|---|
| `Cancelled` | A step of a sequence after one that failed |
| `NoProgress` | A write that completed with no byte written, or on a direct-I/O file a fifth write in a row with no whole block |
| `Partial { done, kind }` | A transfer ended by `kind` after `done` bytes |
| `NotFound`, `Exists`, `Denied`, `NoSpace`, `Unsupported` | `ENOENT`; `EEXIST`; `EACCES` or `EPERM`; `ENOSPC` or `EDQUOT`; `EOPNOTSUPP` |
| `SyncFailed { errno }` | A sync failed, or a durable write with `EIO`, `ENOSPC` or `EDQUOT`; the file is Poisoned |
| `Io { errno }` | `EIO` outside a sync |
| `Invalid { errno }` | `EINVAL` or `EBADF`: after the set's checks, a file system that refuses what was asked (direct I/O), or a bug |
| `Os { errno }` | Any other errno |

`ErrorKind`, inside `Partial`, names the errors a transfer can end with
after bytes were moved: `Poisoned`, `NoProgress`, `NoSpace`, `Unsupported`,
`SyncFailed`, `Io`, `Invalid` and `Os`; any other comes as `Os { errno }`.

`Files::usage()` reports the file slots and operation slots in use, the
operations waiting for room, and `broken_holds`.

## Defaults and what they are from

The defaults of `FileSizes` are a shared slot for every file (with every
file busy, two operation slots a file on average), four path slots,
sequences as long as a file's share, and 64 spans a gathered write. None
is from a measurement: the runs so far size the set themselves.

What has been measured is the set's cost against the raw operations, one
file, one thread, three runs of one second a cell:

- On a Mac's portable backend (`bench/results/m36-file-bench-macos.md`;
  writes and reads of 4 KiB to 1 MiB, one to 16 in flight, buffered and
  direct, synced at the end, every 16 writes or durable each), reads cost
  the set about 1 to 2 % more of the thread's CPU a GB: its median CPU a
  GB is above the raw path's in 75 of 108 read cells and its GB/s below
  in 70, in each of the three runs. That thread also runs the file
  operations themselves, so the figure is a share of the kernel's work
  and the set's together. Writes show no difference: the set is on the
  worse side in 56 of 108 cells. Single cells vary more than that: in 34
  of 432 pairs of a cell and a measure the two APIs' runs do not overlap,
  25 of them against the set.
- On a DGX Spark's NVMe (`bench/results/m37-file-bench-dgx-spark-nvme.md`;
  direct I/O, sequential, 64 KiB and 1 MiB, depth 1 to 32 on the ring, 1
  and 4 on `epoll`), the two paths' ranges overlap in throughput and CPU
  a GB, with the caveats that file gives.

How a program makes its writes durable is its own choice, and these
figures say nothing of it beyond the drive's: on that Mac a sync
(`F_FULLFSYNC`) took 5 to 13 ms, so a sync every 16 writes held either
API to 1,200 to 2,700 writes a second, while durable writes
(`with_dsync`), which there do not flush the drive's cache, ran 4,000 to
44,000 a second, leaving out the buffered 4 KiB sequential cells, whose
passes stalled.

`file_bench` in `crates/longshore-bench` measures the set against the raw
operations on one thread:

```text
file_bench [--backend uring|posix] [--api file|raw|both] [--phase write|read|both]
           [--direct] [--dsync] [--random] [--block N] [--write-size N] [--depth N]
           [--sync-every N] [--file-size N] [--bytes N | --secs S] [--runs N] [--path P]
```

It prints one JSON line a pass, with GB/s, operations a second, the
latency percentiles of the operations and of the syncs, and the thread's
CPU-seconds a GB. The two APIs run in turn on the same file, their order
swapped every run, since whichever runs second finds the file as the first
left it; with `--runs 3` the set goes first in runs 1 and 3.

## How it is checked

| Suite | What it checks | Run |
|---|---|---|
| Contract (`longshore_file::check`) | Every operation and its result, whole transfers, sequences, every refusal submitting nothing, the per-file cap with other files still starting, path slots, first in first out, held-back close, direct-I/O alignment; generic over `IO: Io` | `cargo test -p longshore_file --test file_contract` (`SimIo`, `PosixIo`); `files_uring --dir DIR [--posix]` on a real kernel (`UringIo`, and `epoll`) |
| Model | Seeded operations on the simulated disk in six campaigns (clean, reported and placed faults, failed syncs, torn writes with power loss, power loss, silent faults); invariants F1 to F10 after every step; a failure replays with `FILE_MODEL=campaign:seed` | `cargo test --release -p longshore_file --test file_model` (`FILE_MODEL_SEEDS=n` for more seeds) |
| Allocation guard | Every operation, refusals, waiting work, a sequence, a held-back close, power loss and `power_lost`: nothing allocated after `Files::new` | `cargo test -p longshore_file --test file_alloc` |
| Properties | The plan refuses or adds exactly what its table says and never lets one file take every slot; a write cut short any number of times ends whole; `Unaligned` exactly when a transfer is off a block | `cargo test -p longshore_file --test file_props` |
| Units | Focused cases on a recording backend that logs every submission: a close goes only after the file's other operations, a refused call submits nothing, steps behind a path step wait | `cargo test -p longshore_file --test file_units` |

`longshore_file::check` ships in the library so that `files_uring` can
run it on a real kernel. It is test code and the only part of the crate
that panics: a failed check unwinds with its place and reason, as a failed
assertion does. Nothing in `Files` calls it; a program calls it only from
a test or a self-check.

Faults planted one at a time in the crate, and the suites that caught
them, are in `crates/longshore_file/tests/README.md`, with the rows of the
contract's tables no test reaches: errors no backend here can produce
(`Denied`, a sync failing with an errno other than `EIO`, durable writes
failing with `ENOSPC` or `EDQUOT`), generations wrapping, a short read
before the end of the file, and some sequence steps. The contract, its
state tables and invariants are in
`openspec/changes/archive/2026-11-27-add-file-crate/design.md`; the
requirements in `openspec/specs/file-io/spec.md`.
