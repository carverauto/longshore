# file-io Specification

## Purpose
File I/O through one API, `longshore_file`'s `Files`, the same on io_uring, the portable backend and the simulator: planned at build, handles with generations, whole transfers, ordered sequences, buffers lent without copies, direct I/O checked alike on every backend, and one error type with what a failed sync means. The full contract is in `openspec/changes/archive/2026-11-27-add-file-crate/design.md`.
## Requirements
### Requirement: One API for file I/O
The system SHALL provide a crate, `longshore_file`, re-exported by `longshore` as `file`, whose `Files` set performs file I/O through the `Io` trait and does no I/O itself, so that the same code runs on the io_uring backend, the portable backend and the simulator, driven from the caller's loop on one thread.

#### Scenario: The same program on every backend
- **WHEN** a program drives a `Files` set on `UringIo`, `PosixIo` or `SimIo`
- **THEN** each operation completes with the same outcome on each, as the contract suite checks

#### Scenario: Sharing the ring
- **WHEN** a `Files` set, a `longshore_tcp` set and the S3 client are planned onto one `Limits`
- **THEN** they share one backend and one thread, each taking only its own completions

### Requirement: Planned at build
The system SHALL size every table of a `Files` set when it is planned and built (files, operations in flight, operations waiting, sequence length), allocate nothing after that, and refuse with a typed error what goes beyond a bound.

#### Scenario: Nothing allocated after build
- **WHEN** a set opens, reads, writes, syncs and closes files under the allocation guard
- **THEN** no allocation happens after `Files::new`

#### Scenario: A bound reached
- **WHEN** an operation is asked for while every operation the plan allows is in use
- **THEN** it is refused at once with `FileError::Full`, and nothing is queued past the plan

#### Scenario: One busy file cannot take every slot
- **WHEN** one file has as many operations in flight as its per-file bound allows
- **THEN** its next operation is refused with `FileError::Full`, and every other open file can still start an operation, since each open file has an operation slot reserved for it

### Requirement: Handles and tickets
The system SHALL name each open file by a `File` handle carrying a generation, refuse an operation on a handle whose file was closed, and return for each accepted operation a `Ticket` whose outcome is reported exactly once as an event.

#### Scenario: A stale handle
- **WHEN** an operation names a `File` whose file was closed and whose slot was reused
- **THEN** it is refused with `FileError::Stale` and never reaches the new file

#### Scenario: Every ticket reports once
- **WHEN** operations complete, fail, or are cancelled by a failed sequence
- **THEN** each ticket's outcome is reported exactly once

### Requirement: File operations
The system SHALL offer open (read, write, create, exclusive, direct I/O, durable writes), close, read at an offset, write at an offset, gathered write of several spans, sync of data or of data and metadata, reserve and punch of space, rename, unlink, make a directory, and status.

#### Scenario: Write, sync, read back
- **WHEN** a program writes bytes at an offset, syncs the file and reads them back
- **THEN** the read returns the bytes written

#### Scenario: A gathered write
- **WHEN** a program writes several spans of several buffers as one gathered write
- **THEN** the file holds the spans in order

### Requirement: Whole transfers
The system SHALL resume a read or write that the backend completed short until the transfer is complete, the file ends for a read, or an error ends it, and report how many bytes were transferred.

#### Scenario: A short write resumed
- **WHEN** the backend completes a write short
- **THEN** the set writes the rest, and the event reports the whole length

#### Scenario: An error part-way through
- **WHEN** a resumed write fails after part of it was written
- **THEN** the event reports the error and how many bytes were written before it

### Requirement: Ordered sequences
The system SHALL submit operations that must happen in order as one sequence, perform them in order, and on a failure end the rest of the sequence, each ending operation reporting that it was cancelled.

#### Scenario: Write then sync
- **WHEN** a program submits a write and a data sync as one sequence
- **THEN** the sync starts only after the write completed whole

#### Scenario: A failure ends the rest
- **WHEN** an operation of a sequence fails
- **THEN** the operations after it are not performed and report that they were cancelled

### Requirement: Buffers without copies
The system SHALL transfer file bytes directly from and into the caller's pool buffers, lent through `Extents` and held by the set while an operation uses them, and SHALL NOT copy payload.

#### Scenario: A buffer held during a write
- **WHEN** a program releases its hold on a buffer while a write from it is in flight
- **THEN** the buffer is not given out again until the write has completed

### Requirement: Direct I/O checked the same everywhere
The system SHALL check, before submitting an operation on a file opened for direct I/O, that its buffer is block-aligned and its offset and length are whole blocks, and refuse it otherwise with `FileError::Unaligned` on every backend.

#### Scenario: A misaligned direct write
- **WHEN** a program writes a length that is not whole blocks to a file opened for direct I/O
- **THEN** the write is refused with `FileError::Unaligned`, the same on every backend, and nothing is submitted

### Requirement: One error type and failed syncs
The system SHALL map every backend result to one `FileError`, the same on every backend, and SHALL report a failed sync so that the bytes written since the last good sync are of unknown state, which a later sync does not make durable.

#### Scenario: A failed sync
- **WHEN** a sync fails on the simulated disk
- **THEN** the event reports the failure, the file stays open, and the writes since the last good sync are not reported durable by any later sync

#### Scenario: Power loss
- **WHEN** the simulated machine loses power and the files are read again
- **THEN** every byte an event reported synced is there, and nothing else is promised

### Requirement: Only file I/O
The crate SHALL provide file I/O and nothing built on it: no log, segment, page, record or cache, and no checksum of bodies.

#### Scenario: A log built by its user
- **WHEN** a program needs a write-ahead log
- **THEN** it builds the log from the crate's operations; the crate offers no log of its own

