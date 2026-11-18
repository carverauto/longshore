## ADDED Requirements

### Requirement: File slots
The I/O layer SHALL open files into a table of slots whose size is fixed
when the backend is built, taking the path from a pool buffer, and SHALL
free a slot when its file is closed.

#### Scenario: Open and close
- **WHEN** a file is opened into a free slot and later closed
- **THEN** both operations complete with zero and the slot can be opened again

#### Scenario: Slot in use
- **WHEN** a file is opened into a slot that holds an open file
- **THEN** the open completes with an error and the file already there is untouched

#### Scenario: No such file
- **WHEN** a path that does not exist is opened without the create flag
- **THEN** the open completes with the not-found error and the slot stays free

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
The I/O layer SHALL accept several operations as one chain, all or none,
run them in the order given, and complete those after a failed one as
cancelled.

#### Scenario: Write then sync
- **WHEN** a write and a sync of the same file are submitted as a chain
- **THEN** the sync starts only after the write has succeeded, and each completes once

#### Scenario: Failure in a chain
- **WHEN** the first operation of a chain fails
- **THEN** it completes with its error and each later one completes as cancelled

#### Scenario: No room for the whole chain
- **WHEN** a chain is submitted and fewer operations can be accepted than it holds
- **THEN** it is refused with `Busy` and none of it runs

### Requirement: Space
The I/O layer SHALL reserve space for a range of a file and SHALL punch a
hole in a range, giving its space back, and SHALL offer no operation that
truncates a file.

#### Scenario: Reserve
- **WHEN** space is reserved for a range beyond the end of a file
- **THEN** the file's size covers the range and a read of it returns zeros

#### Scenario: Punch
- **WHEN** a hole is punched in a written range
- **THEN** the file's size is unchanged and a read of the range returns zeros

### Requirement: Names and status
The I/O layer SHALL rename, unlink and make directories by path, and SHALL
report the size of an open file.

#### Scenario: Rename over a file
- **WHEN** a file is renamed to the path of another
- **THEN** the second path names the first file's contents and the first path is gone

#### Scenario: Size
- **WHEN** the status of a file is read after a write that extended it
- **THEN** the decoded size includes the write

### Requirement: Registered memory
The backend SHALL register its pools with the ring as ranges, and building
it SHALL fail, reporting the pool size and the limit, when the pools exceed
the locked-memory limit.

#### Scenario: Limit too low
- **WHEN** a backend is built with pools larger than the locked-memory limit
- **THEN** building fails and the error gives both sizes

#### Scenario: More buffers than sixteen bits can count
- **WHEN** a backend is built with more than 65,536 buffers and an operation uses the last one
- **THEN** the operation reads or writes that buffer

### Requirement: Supported kernels and hosts
Every file operation SHALL work on Linux 6.1 through the ring, and on the
portable backend and in the simulator with the same results.

#### Scenario: Linux 6.1
- **WHEN** the real-kernel file check runs on Linux 6.1
- **THEN** every operation in it succeeds

#### Scenario: Shared tests
- **WHEN** the shared file tests run against the ring, the portable backend and the simulator
- **THEN** each backend passes the same tests
