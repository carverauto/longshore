## Context
The ring backend registers one `iovec` per pool buffer and uses the buffer's
number as the registered index, cut to 16 bits (`io/uring/backend.rs:129`).
The kernel takes at most 16,384 registered buffers, so registration already
fails beyond that. The registered file table holds sockets only and is as
large as `sockets_max`. `ReadFile` is three steps driven by three
completions, on an ordinary descriptor. The pool is one allocation with no
alignment. Results are a 32-bit integer. Every completion comes from the
kernel; the backend cannot complete an operation by itself.

The ring is set up without `IORING_SETUP_SUBMIT_ALL`, and `tick` ignores
how many submissions the kernel consumed. A submission the kernel refuses
to prepare stops the batch, and those behind it are never sent.

`FileId` already names an entry in the list of paths given to
`register_file`.

Facts about Linux 6.1 this design rests on, read from the 6.1 source:
- An open into a registered slot that is in use replaces the file there
  without an error, and refuses the close-on-exec flag.
- A read or write that moves fewer bytes than asked ends a chain of linked
  submissions; so does any failure. Each later member still completes, as
  cancelled.
- A chain does not carry over from one entry to the kernel to the next.
- File status cannot be asked of a registered file.
- A registered buffer may be up to 1 GiB, and an operation may use any
  range inside it.
- Registered memory is charged to the user's locked-memory limit, summed
  over all of that user's rings, and not at all for a process with
  `CAP_IPC_LOCK`.
- The registered file table may not exceed the open-files limit.
- Sync, reserve, punch, rename, unlink, make-directory and an open that
  creates or empties always run on the kernel's worker threads.

## Goals / Non-Goals
- Goals: every file operation a log and a block store need, on Linux 6.1;
  write-then-sync in one submission; no copy between buffer and disk; the
  same operations, results and errors on every backend
- Non-Goals: truncate; directory listing; buffered I/O tuning; reads or
  writes larger than a buffer; disk faults in the simulator, which
  `add-simulated-disk` adds; a thread pool

## Decisions

### Slots, paths and the shared check
- File slots are named by a new type, `FileSlot`. `FileId`,
  `register_file` and `ReadFile` stay as they are, on their ordinary
  descriptor, for the credentials file.
- The registered table holds sockets first and file slots after them. Its
  size is the two counts together. Building fails, with both numbers, when
  that exceeds the open-files limit.
- One module, `io::files`, holds a table with each slot's state (free,
  opening, open) and whether it was opened for direct I/O, and a function
  that checks an operation against the table and `Limits`. The ring
  backend and the engine both call it first in `submit`. It is the only
  place these rules live.
  It refuses:
  - an open into a slot that is not free, with the busy error;
  - any other operation on a slot that is not open or opening;
  - a path that is longer than a buffer, has a zero byte before its end,
    or does not end in a zero byte;
  - on a slot opened for direct I/O, a buffer outside the aligned pool, or
    an offset or length that is not a multiple of the block size;
  - a punch whose offset or length is not a multiple of the block size.
  The direct flag is recorded when the open is submitted, since a write
  may be submitted behind an open still in flight. A failed open returns
  its slot to free.
- An operation the check refuses completes with its error through a short
  list of ready completions that the backend drains in `tick`. The ring
  backend gains that list, and does not wait in the kernel while it holds
  anything.
- A path is bytes in a pool buffer ending in a zero byte, named by buffer,
  offset and length. The kernel copies it when the operation is submitted.
- Open flags are longshore's own: read, write, create, exclusive, empty,
  direct, directory. Their kernel values differ between x86-64 and AArch64
  and are mapped per architecture.

### Operations
- Reads and writes name a slot, a buffer range and a 64-bit offset, and
  complete with the bytes moved, which may be fewer than asked.
- Reserve names a range and whether to keep the file's size or extend it
  to cover the range. Punch gives a range's space back and leaves the size.
- Status is asked by path. The result is written into a pool buffer of at
  least 256 bytes in one layout on every backend, the kernel's, and read
  with a decoder that returns the size.
- There is no truncate. Linux has it on the ring only from 6.9, and the
  alternatives, a higher kernel floor or a blocking call on the I/O
  thread, each break a rule. The open flag "empty" covers a file that is
  rewritten from its start.
- There is no directory listing. No kernel has it on the ring. A caller
  names the files it opens: it probes fixed names and reads the not-found
  error, or keeps a list in a file of its own.

### Chains
- `Io` gains a call that submits a chain. It takes all of the chain or
  refuses it with `Busy`, and needs one free operation slot per member.
- A chain's members are file operations only: read, write, sync, reserve,
  punch, open and close. Each is one submission. Connects, `ReadFile` and
  timers are not allowed in a chain.
- A chain runs in order and ends at the first member that fails or that
  moves fewer bytes than asked. That member completes with its real
  result, the short count included, and each later member completes as
  cancelled. A log that sees a short write finishes the write and submits
  the sync again.
- The shared check runs over the whole chain before any of it is
  submitted, applying each member's effect on the slot table in order. If
  a member is refused, that member completes with its error, every other
  member completes as cancelled, and none reaches the kernel.
- The ring pushes a chain in one call, links all but its last member, and
  enters the kernel only in `tick`, so a chain is never divided.
- A cancel that names a member which has not started completes with a
  negative result and does not stop it. Cancelling the member that is
  running ends the chain.
- The engine runs a chain's members one at a time and applies the same
  ending rule, for both worlds.

### Memory
- The pool gains a third class, aligned buffers. The block size is a
  power of two, at least 4,096, set in `Limits`.
- The pool stays one allocation. It is made one block larger than needed
  and used from the first block boundary inside it, and the aligned class
  starts at a block boundary within the pool. The place of every buffer
  remains a function of `Limits` alone.
- Pools are registered as ranges. A range is the largest whole number of
  buffers that fits in 1 GiB, so no buffer lies across two. An operation
  on a registered buffer names its range and its address. The cut to 16
  bits goes, and the number of buffers is no longer capped at 16,384.
- The kernel judges locked memory, not longshore: the limit is shared by
  every ring of a user and does not apply to a privileged process. When
  the kernel refuses a registration for locked memory, building fails and
  reports the pool's size, the limit, and that the limit is shared.
- The block size is the caller's choice and must suit the device. A
  direct operation the device refuses completes with the kernel's error.

### The ring
- The ring is set up with `IORING_SETUP_SUBMIT_ALL`, and `tick` checks
  that the kernel took every submission.
- Stopping the backend waits a bounded time for operations to end. A sync
  running on a kernel worker cannot be interrupted; if it outlasts the
  bound, the pool is leaked, which is the existing rule for memory the
  kernel may still touch.

### Worlds
- In the POSIX world the calls are `pread`, `pwrite` and their like on
  the thread that calls `tick`. On macOS:
  - direct I/O is the no-cache setting, and alignment is enforced only by
    the shared check;
  - both kinds of sync are `F_FULLFSYNC`, the only call there that reaches
    the device;
  - reserve sets the size where asked to and asks the file system for
    space, which it may give only from the end of the file;
  - punch works on file systems that support it, and otherwise completes
    with the not-supported error.
  Alternative considered: a helper thread. Rejected: it brings a second
  thread and shared buffers for a backend that makes no performance claim.
- In the virtual world a file is bytes in memory with a size and holes. A
  test sets the disk up with a call that fixes the number of files and
  names, the bytes per file and the length of a name, and all memory is
  taken then. Operations are delayed by the existing delay fault, with no
  new draw from the seed. Nothing is lost or torn in this change.

## Risks / Trade-offs
- The time a sync takes on Linux 6.1 follows the kernel's worker threads.
  → The gain of a chained write and sync over two submissions is measured
  on a real disk before it is claimed.
- macOS and Linux differ in what reserve and punch can do. → The shared
  tests cover what both can do; the differences are listed in the README.
- The ready list is a second source of completions in the ring backend.
  → The shared tests cover refusals alone, mixed with kernel completions,
  and inside chains.

## Migration Plan
1. `SUBMIT_ALL` and the consumed-count check. No new operation.
2. `Limits`, the aligned pool, registration by range. The S3 path is
   measured before and after.
3. The slot table, the shared check, the ready list, and the operations:
   ring first, then both worlds.
4. Chains.
Each step is its own commit.

## Open Questions
None.
