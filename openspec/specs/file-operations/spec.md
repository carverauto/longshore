# file-operations Specification

## Purpose
File operations on `Io`: fixed file slots, reads and writes at an offset
from registered and block-aligned buffers, direct I/O, sync, linked chains,
reserving and giving back space, and naming files, on every backend.
## Requirements
### Requirement: File slots
The I/O layer SHALL open files into a table of slots whose size is fixed
when the backend is built, taking the path from a pool buffer, SHALL track
which slots are in use, and SHALL free a slot when its file is closed or
its open fails.

#### Scenario: Open and close
- **WHEN** a file is opened into a free slot and later closed
- **THEN** both operations complete with zero and the slot can be opened again

#### Scenario: Slot in use
- **WHEN** a file is opened into a slot that holds an open file
- **THEN** the open completes with the busy error and the file already there is untouched

#### Scenario: No such file
- **WHEN** a path that does not exist is opened without the create flag
- **THEN** the open completes with the not-found error and the slot is free

#### Scenario: Emptied at open
- **WHEN** a file with contents is opened with the empty flag
- **THEN** a status of its path reports a size of zero

#### Scenario: Path without an end
- **WHEN** an open names a path range that does not end in a zero byte
- **THEN** the open completes with the invalid-argument error and no file is opened

### Requirement: Reads and writes at an offset
The I/O layer SHALL read from and write to an open file at a 64-bit offset,
between the file and a registered buffer, without copying the bytes.

#### Scenario: Write then read
- **WHEN** a buffer is written at an offset and the same range is read into another buffer
- **THEN** the second buffer holds the bytes of the first

#### Scenario: Read past the end
- **WHEN** a read starts at or beyond the end of the file
- **THEN** it completes with zero

#### Scenario: Short read
- **WHEN** a read asks for more bytes than the file has after the offset
- **THEN** it completes with the number of bytes there are

### Requirement: Direct I/O
The I/O layer SHALL provide a pool of buffers aligned to a block size, and
SHALL refuse, with the invalid-argument error and on every backend, an
operation on a file opened for direct I/O whose buffer, offset or length is
not a multiple of that block size.

#### Scenario: Aligned write
- **WHEN** a buffer from the aligned pool is written at an offset that is a multiple of the block size
- **THEN** the write completes with its length

#### Scenario: Unaligned buffer in the simulator
- **WHEN** a buffer from another pool is written to a file opened for direct I/O in the simulator
- **THEN** the write completes with the invalid-argument error

#### Scenario: Write behind an open in flight
- **WHEN** a misaligned write is submitted to a slot whose direct open has not yet completed
- **THEN** the write completes with the invalid-argument error

### Requirement: Sync
The I/O layer SHALL sync an open file, either its data alone or its data
and metadata, and complete only when the device has confirmed it.

#### Scenario: Data sync
- **WHEN** a file is written and then synced for data
- **THEN** the sync completes with zero after the write

#### Scenario: Directory sync
- **WHEN** a directory is opened and synced after a file in it was renamed
- **THEN** the sync completes with zero

### Requirement: Chains
The I/O layer SHALL accept several file operations as one chain, all or
none, run them in the order given, and end the chain at the first member
that fails or moves fewer bytes than asked, completing each later member as
cancelled.

#### Scenario: Write then sync
- **WHEN** a write and a sync of the same file are submitted as a chain and the write moves every byte
- **THEN** the sync starts only after the write, and each completes once

#### Scenario: Short write in a chain
- **WHEN** the write of a write-then-sync chain moves fewer bytes than asked
- **THEN** the write completes with the bytes it moved and the sync completes as cancelled

#### Scenario: Failure in a chain
- **WHEN** the first operation of a chain fails
- **THEN** it completes with its error and each later one completes as cancelled

#### Scenario: Refused member
- **WHEN** a chain holds a member the shared check refuses
- **THEN** that member completes with its error, every other member completes as cancelled, and no file is changed

#### Scenario: No room for the whole chain
- **WHEN** a chain is submitted and fewer operations can be accepted than it holds
- **THEN** it is refused with `Busy` and none of it runs

#### Scenario: Chain among other submissions
- **WHEN** a chain and other operations are submitted in the same turn
- **THEN** the chain's order holds and the other operations are not cancelled with it

### Requirement: Space
The I/O layer SHALL reserve space for a range of a file, either keeping
its size or extending it to cover the range, SHALL punch a hole in a
block-aligned range, giving its space back, and SHALL offer no operation
that truncates a file or lists a directory.

#### Scenario: Reserve and extend
- **WHEN** space is reserved, with the size extended, for a range beyond the end of a file
- **THEN** the file's size covers the range and a read of it returns zeros

#### Scenario: Reserve and keep the size
- **WHEN** space is reserved, with the size kept, for a range beyond the end of a file
- **THEN** the file's size is unchanged

#### Scenario: Punch
- **WHEN** a hole is punched in a written, block-aligned range
- **THEN** the file's size is unchanged and a read of the range returns zeros

#### Scenario: Punch off a block boundary
- **WHEN** a punch names a range that is not a multiple of the block size
- **THEN** it completes with the invalid-argument error on every backend

### Requirement: Names and status
The I/O layer SHALL rename, unlink and make directories by path, and SHALL
report the size of a file by path in one layout on every backend.

#### Scenario: Rename over a file
- **WHEN** a file is renamed to the path of another
- **THEN** the second path names the first file's contents and the first path is gone

#### Scenario: Size
- **WHEN** the status of a path is read after a write that extended its file
- **THEN** the decoded size includes the write

#### Scenario: Probing for a file
- **WHEN** the status of a path that does not exist is read
- **THEN** it completes with the not-found error

### Requirement: Registered memory
The ring backend SHALL register its pools as ranges of whole buffers, and
building it SHALL fail with a message that gives the pool size and the
limit when the kernel refuses a registration for locked memory or the file
table exceeds the open-files limit.

#### Scenario: Locked memory refused
- **WHEN** an unprivileged process builds a backend whose pools exceed its locked-memory limit
- **THEN** building fails and the error gives the pool size and the limit

#### Scenario: More buffers than the kernel registers singly
- **WHEN** a backend is built with more than 16,384 buffers and an operation uses the last one
- **THEN** the operation reads or writes that buffer

#### Scenario: File table too large
- **WHEN** the socket and file slots together exceed the open-files limit
- **THEN** building fails and the error gives both numbers

### Requirement: Every submission is sent
The ring backend SHALL send every queued submission in the turn it was
queued, whatever the kernel does with one of them.

#### Scenario: A path the kernel refuses
- **WHEN** an operation whose path the kernel refuses is queued ahead of others
- **THEN** it completes with an error and the others complete as they would alone

### Requirement: Supported kernels and hosts
Every file operation SHALL work on Linux 6.1 through the ring, and on the
portable backend and in the simulator with the same results, except that on
macOS a reserve MAY set aside less space than asked and a punch MAY
complete with the not-supported error.

#### Scenario: Linux 6.1
- **WHEN** the real-kernel file check runs on Linux 6.1
- **THEN** every operation in it succeeds

#### Scenario: Shared tests
- **WHEN** the shared file tests run against the ring, the portable backend and the simulator
- **THEN** each backend passes the same tests

### Requirement: Durable writes
A file SHALL be openable so that every write to it completes only when its
data, and the metadata needed to read it back, is on stable storage
(`OpenFlags::with_dsync`, `O_DSYNC`), on the ring and on the portable
backend, with no sync operation of its own. The file's name is not covered:
a newly created file is there after a power loss only once its directory has
been synced. On macOS the drive's own cache is not flushed, and the
documentation SHALL say so.

#### Scenario: Written and read back
- **WHEN** a file is opened for durable writes on any backend and written at two offsets
- **THEN** the writes complete with their lengths, and both are read back through the same file and through a second, read-only open

#### Scenario: A log on the ring and on epoll
- **WHEN** a log node writes each block to a preallocated file opened for durable writes with direct I/O, on the ring and on the portable backend
- **THEN** every block is answered after its write completes, with no sync submitted

