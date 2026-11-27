## Context
`longshore_common` offers file I/O as operations of the `Io` trait
(`Op::FileOpen` to `Op::FileMkdir`, `Op::FileWriteSpans`), fixed file slots,
direct I/O, chains (`Io::submit_chain`) and a simulated disk with faults and
power loss (specs `file-operations`, `simulated-disk`). What it does not
offer is a layer that owns the plumbing around them, as `longshore_tcp`'s
`Net` does for sockets. This change adds that layer, for files only.

## Goals / Non-Goals
- Goals: one API for file I/O over every backend; handles that cannot reach
  a reused slot; whole transfers; ordered sequences; direct I/O checked the
  same way everywhere; errors the same everywhere; nothing allocated after
  build; tested on all three backends and under every fault the simulated
  disk has.
- Non-Goals: anything a user of files builds on top (logs, segments, pages,
  records, caches, recovery of a format); checksums of bodies; a page cache;
  an async runtime; truncate and directory listing.

## Decisions
- **The crate sits where `longshore_tcp` sits.** A layer of state machines
  over the `Io` trait that does no I/O itself (first principle 5): the
  same code runs on `UringIo`, `PosixIo` and `SimIo`. The caller owns the
  backend and the loop.
- **The same driving API as `Net`.** `FileConfig::plan(limits, sizes)`
  returns the `Limits` with the set's file slots, operations and tokens
  added, and the set's share of them; `Files::new(config, &limits)` makes
  every table once; `on_completion(io, c) -> bool` takes the set's
  completions; `poll(io, now)` submits what waits; `next_event()` reports
  outcomes. Sets of files, connections and the S3 client then share one
  ring and one thread.
- **Tickets, not tokens.** Each operation returns a `Ticket` (slot and
  generation) or a refusal at once (`Full`, `Stale`, `Unaligned`,
  `TooLarge`, `Closed`); its outcome is an event naming the ticket. The set
  keeps the backend's tokens to itself.
- **Whole transfers.** A read or write is resumed after a short completion
  until it is done, the file ends (a read) or an error ends it; the event
  says how much was transferred. A caller that wants the raw behaviour has
  `longshore_common`'s operations.
- **Ordered sequences on the backend's chains.** A sequence (for example a
  write then a data sync) is submitted as one chain; a failure cancels the
  rest, and every operation of it still reports. Bounded by the chain room
  the plan gives.
- **Memory through `Extents`.** Buffers are the block-aligned class of the
  pool, taken and held through `longshore_common`'s `Extents`, so a buffer
  written to a file and sent on a socket is the same buffer, held by each
  until done. The set holds a buffer for as long as an operation reads or
  writes it.
- **Alignment checked before submission.** For a file opened for direct
  I/O, offsets, lengths and buffers are checked against the block size
  before anything is submitted; every backend then sees only what it would
  take.
- **Errors made one.** Backend results map to one `FileError`. A failed
  sync is stated precisely: the bytes written since the last good sync are
  of unknown state, the file stays open, and a later sync does not make
  them good (the semantics the simulated disk already models).
- **Bounded everything.** Files, operations in flight, operations waiting
  for room, sequence lengths: all sized by the plan, refused beyond it.

## The contract

Revised after its review (task 0.2): every point below rests on what the
backends and the simulated disk do, with the file that shows it.

### C1 Layer, and where the set submits
`longshore_file` is a set of state machines over `longshore_common::io::Io`.
It never blocks, sleeps, spawns or reads a clock, and it has no timers. It
submits in three places only: the call that asks for an operation (when it
can go at once), `on_completion` (a resumed transfer, a held-back close, the
next step of a sequence), and `poll` (work that waited for room).

### C2 Plan and bounds
`FileSizes::new(files, ops_per_file)` with setters `with_shared(n)`,
`with_paths(n)`, `with_sequence_max(n)`, `with_spans_max(n)`,
`with_token_base(t)`. `FileConfig::plan(limits, sizes) -> Result<(Limits,
FileConfig), ConfigError>` adds to `limits`:

| Count | Planned |
|---|---|
| operation slots `ops` | `files` reserved (one a file slot) + `paths` (one a path operation) + `shared` |
| `Limits::ops_max` | + `ops` |
| `Limits::files_max` | + `files` |
| `Limits::gather_ops` | + `ops` (a row for every operation slot), `gather_spans = max(existing, spans_max)` |
| small buffers | + `files` (each file slot's path, for its open) + `paths` (each path operation's paths and status area) + `ops` (a span list for every operation slot) |
| tokens | `tokens()` from `token_base`, one a record: `ops` operation records and one a file slot for its open and close (`FileConfig::tokens`) |

The small class's size is the pool's (shared with other layers, as
`longshore_tcp` does): the plan refuses one that cannot hold a list of
`spans_max` spans (12 bytes each) or a 256-byte status area and one byte of
path. A path holds `path_max = (small_buffer_size − 256) / 2 − 1` bytes (two
paths fit one buffer for a rename, the status area after them).

**No file can take every slot.** An open file uses its reserved slot first,
then shared slots, up to `ops_per_file` at once; a path operation uses only
its own path slots; a file's open uses its own path buffer and reserved
slot. So every open file can always start one operation, path operations
never take a file's share, and one busy file holds at most `ops_per_file −
1` shared slots. Gather rows and span lists are one per operation slot, so
no file's gathered writes can use up another's. The plan refuses:
`ops_per_file` of 0 or above `shared + 1` (`ConfigError::Ops`),
`sequence_max` above `ops_per_file` (`ConfigError::Sequence`), `spans_max`
of 0 or above 1,024 (`ConfigError::Spans`), a small class too small
(`ConfigError::ClassSize`).

### C3 A file slot
`File { slot, generation }`:

| State | Entered by | Accepts | Left by |
|---|---|---|---|
| Free | build; a close's completion; a failed open; `power_lost` | `open`, once the close's or failed open's event was taken | `open` accepted → Opening |
| Opening | `open` accepted | nothing | the open's completion: success → Open, failure → Free (generation + 1) |
| Open | the open's success | every operation | `close` → Closing; a failed sync, or a failed write on a durable-write file → Poisoned |
| Poisoned | as above | `read_at` and `close`; the rest `Poisoned` | `close` → Closing |
| Closing | `close` accepted | nothing (`Closing`) | the close's completion → Free (generation + 1) |

**Close is held back** until every other operation of the file has
reported, then submitted in the file's reserved slot's room at the
backend; `close` is never `Full`. Opens and closes have ticket records of
their own, one a file slot (indexes `ops` to `ops + files − 1`), so a
reserved slot still holding an untaken event never makes them wait. A
Free file slot is opened again only once its close's (or failed open's)
event has been taken. A close error is reported in its event and the slot
is freed all the same. Generations are `u32` and wrap.

### C4 Operations, tickets and events
Each accepted operation takes a record and returns `Ticket { index,
generation }`: an operation slot (indexes 0 to `ops − 1`), or, for an open
or a close, its file slot's own record (`ops` and above). `Ticket::NONE`
names no record and fills the slice given to `sequence`. States of an
operation record:

| State | Entered by | Left by |
|---|---|---|
| Free | build; its event taken; `power_lost` | accepted → Waiting or Submitted |
| Waiting | accepted without room at the backend (`Busy`), or a sequence step behind an earlier one | room: → Submitted (first in, first out) |
| Submitted | given to the backend | completion: whole → Reported; short → Resuming; failed → Reported |
| Resuming | a short transfer, or a broken chain | every member of the broken chain reported → Submitted (the rest) |
| Reported | its outcome decided | `next_event` returns its event → Free |

The outcome is one `Event::Done { ticket, file: Option<File>, result:
Result<u64, FileError> }`; `u64` is bytes transferred, the size for
`status`, 0 otherwise; `file` is the new handle for `open`, the file for
file operations, `None` for path operations. The event queue holds `ops +
files` events, one a record; a record is free only when its event is
taken, so the queue never overflows. Waiting work goes first in, first out: a sequence waiting for
room is not overtaken, so nothing waits for ever while room comes back.

### C5 Operations
| Operation | Needs | Holds while in flight | `u64` of the result |
|---|---|---|---|
| `open(path, OpenFlags)` | a free file slot | its path buffer | 0 (the `File` is in `file`) |
| `close(file)` | – | – | 0 |
| `read_at(file, extent, at, len, file_off)` | not opened write-only | the extent | bytes read; fewer only at end of file |
| `write_at(file, extent, at, len, file_off)` | opened for writing | the extent | bytes written: all, or an error |
| `write_spans(file, &[Span], file_off)` | opened for writing | each extent span; the slot's span list | as `write_at` |
| `sync(file, data_only)` | – | – | 0 |
| `reserve(file, off, len, keep_size)` | opened for writing, `len > 0` | – | 0 (best effort on macOS) |
| `punch(file, off, len)` | opened for writing, `len > 0`, whole blocks | – | 0 |
| `status(path)` | a path slot | its path buffer and status area | the size |
| `rename(from, to)`, `unlink(path)`, `mkdir(path)` | a path slot | its path buffer | 0 |

A directory is a file: `open` with `OpenFlags::with_directory` and `sync`
it, to make a create or a rename durable. `Span` is `Span::Extent { extent,
at, len }`, held by the set, or `Span::Page { page, off, len }`, a page of
the receive area (`Limits::is_page`, else `TooLarge`) that the caller holds (`longshore_tcp`'s `WriteHold`)
until the event: a received frame goes to a file without a copy. Paths are
copied into path buffers (a path is not payload); a path longer than
`path_max` or with a NUL inside is refused `BadPath`. There is no cancel:
an operation the kernel never completes keeps its file Closing for ever.

### C6 Whole transfers
A short read or write is resumed for the rest from `on_completion`,
keeping its ticket. Every resume moves at least one byte: a write that
completes with 0 bytes, or a read with 0 before the end it asked for, ends
the transfer (a read: at end of file; a write: `FileError::NoProgress`).
On a direct-I/O file a short write resumes from what was written rounded
down to a block (the same bytes rewritten from the same buffer); one that
wrote less than a block is rewritten from the same offset, four times at
most: a fifth such write in a row with no whole block ends `NoProgress`,
so resumes are bounded; `done` then counts whole blocks only. Whenever a
write ends in an error, the bytes of its range past `done` are of unknown
state: the backend may have written some of them. A short read ending off a block boundary is the end of the file. An error
ends the transfer with `FileError::Partial { done, kind }` when bytes were
done, the plain error when none were.

### C7 Ordered sequences
`sequence(&[Step]) -> Result<[Ticket], FileError>` (tickets written into a
caller's slice) takes up to `sequence_max` steps; a step is any operation
but `open` and `close`, of any file or a path. Each step counts against its
own file's share or the path slots; the whole sequence is refused `Full`
unless every step has room. The set pre-checks every step, so the backend
never refuses the chain for its operations. Should it refuse one for
want of its own resources all the same (another layer took room the plan
gave the set), the refused step reports its own error as C10 maps it
(`ENOBUFS` as `Os { errno }`), the others `Cancelled` (a step resumed in
that chain that had moved bytes before, `Partial { done, kind: Os {
errno: ECANCELED } }`, as C6 has it), and no file is poisoned by it. A
step of a file poisoned while it waited ends `Poisoned` once the backend
took the steps before it, never before: a close it held back then goes
behind them, and a chain the backend refuses for room waits whole.
Steps go to the backend as chains: a chain ends after every path step
(status, rename, unlink, mkdir), and the steps after it are submitted as a
new chain only once it reported success, since the kernel does not break
a link on every failed path operation (`statx` does not; found by the
contract suite on `UringIo`). When the backend ends a chain early
because a transfer was short, the set waits until every member has
reported, resumes the short one, and submits it and the rest as a new
chain. When a step fails, the steps after it report `Cancelled`. Only a
sequence orders operations: independent operations, even of one file, run
in any order.

### C8 Memory
Extents are of the caller's `Extents` (block-aligned class), passed as
`&mut Extents` to every call that names one, to `on_completion` and to
`power_lost`. The set holds each extent an operation names from its
acceptance to its report; a hold that fails is refused at the call
(`StaleExtent`, `TooManyHolds`). If the set's own release fails (the caller
released the set's hold), that is a broken invariant: debug-asserted and
counted. No payload is copied.

### C9 Checks before submission
On a direct-I/O file: each extent span is of the block-aligned class and
starts at a block boundary; lengths and file offsets are whole blocks;
otherwise `Unaligned`. On every file: `punch` is whole blocks, otherwise
`Unaligned`; `reserve`
and `punch` have `len > 0`; ranges lie inside their extents; a transfer
(`read_at`, `write_at`, or a gathered write's spans together) moves at
most `i32::MAX` bytes, since every backend counts a completion's bytes in
an `i32` (`TooLarge`).

### C10 Errors and poisoning
`FileError` is flat (no field of its own type): `Full`, `Stale`,
`Closing`, `Poisoned`, `NotOpen`, `Unaligned`, `TooLarge`, `BadPath`,
`StaleExtent`, `TooManyHolds`, `Cancelled`, `NoProgress`, `Partial { done,
kind: ErrorKind }`, and the backend's: `NotFound` (`ENOENT`), `Exists`
(`EEXIST`), `Denied` (`EACCES`, `EPERM`), `NoSpace` (`ENOSPC`, `EDQUOT`),
`Unsupported` (`EOPNOTSUPP`), `SyncFailed { errno }`, `Io { errno }`,
`Invalid { errno }` (`EINVAL`, `EBADF`: impossible after C9, a bug),
`Os { errno }`. `ErrorKind` names the errors a transfer can end with
after bytes were done: `Poisoned`, `NoProgress`, `NoSpace`, `Unsupported`,
`SyncFailed`, `Io`, `Invalid`, `Os`; the others come as `Os { errno }`.

**Poisoning.** Every failure of a sync, whatever its errno (the portable
backend reports unknown errors as `EIO`, the ring passes the kernel's, and
a failed write-back can come as `ENOSPC`), except `Unsupported`, is
`SyncFailed` and poisons the file; so does a write on a file opened for
durable writes (`OpenFlags::with_dsync`) that fails with `EIO`, `ENOSPC` or
`EDQUOT`, which is a failed sync and reports `SyncFailed` (or `Partial {
done, kind: SyncFailed }` when bytes were done); other errors of such a
write are reported as they are and do not poison, and neither does a
write that ends `NoProgress`. A Poisoned file's waiting operations end `Poisoned`, its transfers
in progress end `Partial { done, kind: Poisoned }`, where `done` is every
byte the backend reported, a direct write's part of a block included
(plain `Poisoned` only when the backend wrote nothing), and its operations
in flight report their own outcomes. Reads stay allowed: the bytes are
readable, only their durability is lost.

### C11 Refusals at the call
`Full`, `Stale`, `Closing`, `Poisoned`, `NotOpen`, `Unaligned`, `TooLarge`,
`BadPath`, `StaleExtent`, `TooManyHolds`. A refusal submits nothing, takes
no slot and no hold. A transfer of more than `i32::MAX` bytes (C9) is
refused `TooLarge` at the call, as a step too, so no backend is given
one. A refused sequence leaves the tickets of the steps before the
refused one `Ticket::NONE` in the caller's slice.

### C12 Power loss, and what is not detected
`Files::power_lost(&mut Extents)` frees every file and operation slot,
raises every generation, drops waiting work and events not yet taken,
releases every hold, and
allocates nothing; the tickets in flight end without events. The caller
calls it when its backend lost power (`SimIo::power_loss`) and then opens
the files again. Silent faults (a lost, torn or misdirected write reported
done, a flipped bit) are not detected: that takes checksums of the bytes,
which the crate leaves to its users.

### Invariants, checked after every step of the model test
- F1 Nothing is submitted for a stale handle, or for a file slot that is
  not Open, Poisoned (a read) or Closing (the operations accepted before the
  close), but its own open and its held-back close.
- F2 Every accepted ticket reports exactly once, unless `power_lost` ended
  it; no event names an unknown ticket.
- F3 Every extent named by an operation not yet reported is held by the
  set; the set holds nothing for an operation that reported.
- F4 A file's operation slots in use never exceed `ops_per_file`; path
  operations use only path slots; every Open file has a slot it can use.
- F5 The steps of a sequence each start after the one before is whole;
  after a failed step the rest report `Cancelled`. Their events need not
  come in step order: a step waiting behind an earlier one ends `Poisoned`
  when its chain comes up for submission and its file is poisoned by then,
  and the steps after it `Cancelled`, which can be before a step of another
  file reports; within one turn a poisoned file's
  waiting operations may report before the failed sync that poisoned it.
- F6 A Poisoned file submits nothing but reads and its close.
- F7 With no silent fault on, a read returns, for each byte, what the last
  write that completed before the read was submitted put there, when no
  write of that range is in flight.
- F8 With no silent fault on, after a power loss every byte is there that
  was written by a write that completed whole, followed by a sync of that
  file submitted after the write completed and reported success, with no
  failed sync of that file through any handle after the write, and the
  file's name made durable: after its create, a good sync of its directory;
  after a rename, good syncs of both the old and the new directory; and
  every directory above it durable the same way (a write to a durable-write
  file counts as synced when it completes).
- F9 Nothing is allocated after `Files::new`, `power_lost` included.
- F10 At rest with the event queue empty, every slot's state agrees with
  the events taken: no file slot Opening or Closing, no operation slot in
  use.

### The API, for the parallel work
```rust
pub struct FileSizes;   // new(files, ops_per_file), with_shared, with_paths,
                        // with_sequence_max, with_spans_max, with_token_base
pub struct FileConfig;  // plan(limits, sizes) -> Result<(Limits, FileConfig), ConfigError>,
                        // tokens(), token_base()
pub enum ConfigError { Ops, Sequence, Spans, ClassSize, Tokens }
pub struct Files;       // new(config, &limits)
pub struct File { /* slot, generation */ }
pub struct Ticket { /* index, generation */ }
pub enum Span { Extent { extent, at, len }, Page { page, off, len } }
pub enum Step { Read{..}, Write{..}, WriteSpans{..}, Sync{..}, Reserve{..},
                Punch{..}, Status{..}, Rename{..}, Unlink{..}, Mkdir{..} }
pub enum Event { Done { ticket: Ticket, file: Option<File>, result: Result<u64, FileError> } }
pub enum FileError { /* C10 */ }   pub enum ErrorKind { /* C10 without Partial */ }

// The skeleton in crates/longshore_file/src/files.rs is the authority for
// these signatures. `Range = (Extent, at: u32, len: u32)`; space ranges
// are `(off: u64, len: u64)`; tuples keep every call under clippy's
// argument limit without a lint exemption.
impl Files {
    pub fn open<IO: Io>(&mut self, io: &mut IO, path: &str, flags: OpenFlags) -> Result<Ticket, FileError>;
    pub fn close<IO: Io>(&mut self, io: &mut IO, file: File) -> Result<Ticket, FileError>;
    pub fn read_at<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, file: File, range: Range, file_off: u64) -> Result<Ticket, FileError>;
    pub fn write_at<IO: Io>(/* as read_at */) -> Result<Ticket, FileError>;
    pub fn write_spans<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, file: File, spans: &[Span], file_off: u64) -> Result<Ticket, FileError>;
    pub fn sync<IO: Io>(&mut self, io: &mut IO, file: File, data_only: bool) -> Result<Ticket, FileError>;
    pub fn reserve<IO: Io>(&mut self, io: &mut IO, file: File, range: (u64, u64), keep_size: bool) -> Result<Ticket, FileError>;
    pub fn punch<IO: Io>(&mut self, io: &mut IO, file: File, range: (u64, u64)) -> Result<Ticket, FileError>;
    pub fn status / unlink / mkdir<IO: Io>(&mut self, io: &mut IO, path: &str) -> Result<Ticket, FileError>;
    pub fn rename<IO: Io>(&mut self, io: &mut IO, from: &str, to: &str) -> Result<Ticket, FileError>;
    pub fn sequence<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, steps: &[Step<'_>], tickets: &mut [Ticket]) -> Result<usize, FileError>;
    pub fn on_completion<IO: Io>(&mut self, io: &mut IO, x: &mut Extents, c: Completion) -> bool;
    pub fn poll<IO: Io>(&mut self, io: &mut IO, x: &mut Extents);
    pub fn next_event(&mut self) -> Option<Event>;
    pub fn power_lost(&mut self, x: &mut Extents);
    pub fn usage(&self) -> FileUsage; // files(), ops(), waiting(), broken_holds()
}
```

## Risks / Trade-offs
- An API that does too much becomes a storage engine → the non-goals above
  are part of the spec, and the review of each group checks against them.
- Resuming short transfers hides partial progress → the event says how much
  was transferred, and a failure part-way through says so.
- Chains differ between backends (the ring links operations, the portable
  backend and the simulator order them in the engine) → the contract suite
  runs every sequence on all three.

## Migration Plan
Nothing to migrate: the crate is new, and `longshore_common`'s operations
stay.

## Open Questions
- None open: the per-file bound is C2 (a reserved operation slot for every
  file and a cap per file), as the user asked.
