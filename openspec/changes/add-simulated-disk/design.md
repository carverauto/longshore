## Context
After `add-file-operations` the virtual world holds each file as bytes in
memory, set up by a call that fixes every capacity, and every operation
succeeds. Network faults are rates in parts per million in `Faults`, drawn
from the one generator with `chance`, which draws nothing for a rate of
zero. A run is identified by its trace hash. The client and its test server
share one simulator.

UltraGraphDB's `SimStorage` keeps a buffered and a stable view of each file
and directory. It offers power loss (everything reverts to stable), a fault
at a numbered operation, random faults at one rate, corruption of the
medium and a refused preallocation. Its fault modes are before, after,
torn, silently torn and short-torn. It is written as futures that complete
when first polled, allocates on every operation, and has a generator of its
own. Its files describe the model as following Apache Iggy's simulated
storage.

## Goals / Non-Goals
- Goals: every way a disk fails that a log or block store must survive;
  the same seed gives the same disk; files and sockets under one clock;
  nothing a real device or Linux could not do; no allocation after setup
- Non-Goals: simulating several machines, with one losing power while its
  peers run or with messages dropped, duplicated and reordered between
  them, which is UltraGraphDB's own simulation and stays there; a file
  system's journal or a device's wear;
  performance; faults on the portable backend's real files

## Decisions

### State
- A file has durable bytes, with a size and holes, and a log of the writes
  made since its last sync. A read sees the durable bytes with the log
  laid over them, in order. A sync folds the log into the durable bytes.
  Alternative considered: two byte arrays, written and durable. Rejected
  because overlapping unsynced writes could not be settled one by one at a
  power loss: the earlier write's bytes would already be overwritten.
- The setup call fixes the number of files and names, the bytes per file,
  the number of unsynced writes and their total bytes, the sectors that
  can hold a read error, and the sector size. All memory is taken then.
- When the log of unsynced writes is full, the oldest is made durable, as
  the kernel's writeback would do, and the new write is accepted.
- A directory has durable names and the changes made since its last sync.
  A rename, an unlink or a new file is durable only after the directory is
  synced. A file that is open survives a rename or unlink of its name.
- The sector size is 512 or 4,096. Faults cut and place writes on sector
  boundaries of the file.

### Power loss
- Power loss is a call on the simulator, not an operation. It ends the
  whole simulated machine:
  - every operation in flight is dropped without a completion, on sockets
    as well as files. This is the one case where an accepted operation
    gets none;
  - every socket and file slot becomes free, timers are gone, and the
    waker flag is cleared;
  - each unsynced write, in order, is kept whole, dropped, or kept up to a
    sector boundary inside it, by the seed. A write that lies within one
    sector is kept or dropped. Unsynced directory changes are each kept or
    dropped;
  - a sector with a read error keeps it.
- The test then builds its callers again, on both ends of any connection,
  and opens the files. Completions it had already taken belong to callers
  that no longer exist.
- Operation numbers for placed faults carry on across a power loss.

### Faults
- Disk faults are a `DiskFaults` value set through a call of its own, so
  `SimIo::new` keeps its signature. Every rate is drawn with `chance`.
  With every rate at zero the generator is not consulted, and the traces
  of existing tests do not change.
- Write faults, each with a rate:
  - short: completes with success and a count below what was asked, cut at
    a sector boundary. In a chain it ends the chain, as on the kernel;
  - torn, reported: completes with the I/O error; a prefix up to a sector
    boundary is written;
  - torn, silent: completes with the full count; only such a prefix is
    written;
  - lost, reported: completes with the I/O error; nothing is written;
  - lost, silent: completes with the full count; nothing is written.
  The silent ones exist because a store must detect them by its own
  checksums.
- A misdirected write completes with the full count. Its bytes land at
  another sector-aligned offset inside the file's size, and the range it
  named keeps its old bytes. A file with no other place for it is not
  misdirected.
- A read error marks one sector. Reads that touch it complete with the I/O
  error until a write or a punch covers the whole sector.
- A flipped bit on read changes the bytes delivered, not the medium. A
  test call flips a bit on the medium itself, in the durable bytes and the
  log alike.
- A failed sync completes with the I/O error. The writes it covered stay
  readable but are marked as never to become durable: a later sync that
  succeeds does not bring them along, and a power loss drops them. This is
  what Linux does with the pages of a failed sync.
  Alternative considered: keep the writes for a retry. Rejected because it
  is what stores once assumed and what Linux does not do.
- A refused reserve completes with the no-space error and a refused punch
  with the not-supported error.
- A placed fault names a file operation by its number, counted when it is
  submitted, and a kind. It overrides the rates for that operation.
  - "fail" completes the operation with the I/O error and without effect;
  - "fail after" lets the operation take effect and then completes it
    with the I/O error;
  - the five write faults and the misdirected write, which on an
    operation that is not a write act as "fail".
- A counter per fault kind tells a test how often each was injected.

### From UltraGraphDB
- The source is the crate `ultragraphdb_transactions_log`, in
  UltraGraphDB's `ultragraphdb_transactions/` directory. Paths below are
  inside that crate.

  | File | Lines | Holds | Here |
  |---|---|---|---|
  | `src/types/sim_storage/sim_state.rs` | 378 | The model: two views per file and directory, sync, power loss, fault injection | Rules carried over, code written anew |
  | `src/types/sim_storage/mod.rs` | 133 | The calls a test makes: power loss, a fault at an operation, random faults, corruption, a refused preallocation | Written anew as setup calls on the simulator |
  | `src/types/sim_storage/sim_inode.rs` | 47 | A file that outlives its name | Rule carried over |
  | `src/types/sim_storage/sim_tear.rs` | 12 | Where a torn write is cut | Carried over |
  | `src/types/fault_mode.rs` | 25 | The fault modes | Mapped onto the faults of this design |
  | `src/types/storage_operation.rs` | 35 | The kinds of operation a fault is placed on | Replaced by `Io`'s operations |
  | `tests/types/sim_storage/sim_storage_tests.rs` | 672 | About thirty tests of the model | Written again against `Io` |

- Not taken: the glue to UltraGraphDB's storage traits
  (`sim_storage_durable_storage.rs`, `src/types/sim_file/`), its aligned
  buffer, the traits themselves, and its tests of the log. Those stay in
  UltraGraphDB, which later puts an implementation over `Io` in their
  place. The crate `ultragraphdb_store_sim` simulates the key-value store
  above the disk and is not part of this.
- What carries over is the rules: two states, a sync that joins them,
  names durable only through a directory sync, a file outliving its name,
  and the tear arithmetic. The code is written anew.
- Where this model differs from `SimStorage`, and why:

  | `SimStorage` | Here | Why |
  |---|---|---|
  | Power loss reverts everything to stable | Each unsynced write settled by the seed | A real disk keeps some of them |
  | A write of a sector or less can tear inside the sector | It is kept or dropped | A sector is written whole or not |
  | A failed sync keeps the buffered data for a retry | The data is never made durable | Linux's behaviour |
  | Short-torn write: an error, and a prefix | Short write: success and a count; torn reported: error and a prefix | The two cases a kernel can return |
  | One rate, equal odds between modes | A rate per fault | Tests can isolate one fault |
  | Truncate, list, length by handle | None; status by path | `add-file-operations` has none |
  | A log of every operation | Counters per fault | Nothing allocates |
  | Its own generator | The simulator's | One seed for network and disk |

- Its tests are written again as operations and completions. Those that
  pin its generator's draws, read its operation log, or use truncate or
  list have no counterpart and are listed in the pull request.
- The new files carry this project's header. Their comments and `NOTICE`
  record that the model comes from UltraGraphDB, which followed Apache
  Iggy's simulated storage.

## Risks / Trade-offs
- The model may allow a state no real disk produces, and a store would
  then be hardened against nothing. → Each fault's comment names the real
  failure it stands for.
- A power loss takes the simulated peer down too. → A test of a client
  against a surviving server is a network-fault test, which exists.
- The write log makes the simulator's memory depend on a setting the test
  must choose. → The oldest write is made durable when it fills, so a
  small log changes what can be lost, never whether the test runs.

## Migration Plan
1. Durable bytes, the write log and sync, with no faults: existing file
   tests and traces unchanged.
2. Directories.
3. Power loss.
4. Faults by rate, then placed faults.
UltraGraphDB moves to this model after step 4, in its own repository.

## Open Questions
None.
