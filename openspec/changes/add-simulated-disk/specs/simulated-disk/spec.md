## ADDED Requirements

### Requirement: Durable and written state
The simulator SHALL keep, for each file and directory, what is durable
apart from what has been written since, SHALL make writes durable when a
sync of that file or directory completes with success, and SHALL take all
of the disk's memory when the test sets the disk up.

#### Scenario: Synced write survives
- **WHEN** a write is synced and power is then lost
- **THEN** a read after the files are opened again returns the write

#### Scenario: Read sees unsynced writes
- **WHEN** a file is written and read back without a sync
- **THEN** the read returns the written bytes

#### Scenario: Rename without a directory sync
- **WHEN** a file is renamed, its directory is not synced, and power is lost
- **THEN** the file is found under its old name or its new one, as the seed decides

#### Scenario: More unsynced writes than the disk was set up for
- **WHEN** a file receives more unsynced writes than the configured number
- **THEN** every write is accepted and the oldest become durable

#### Scenario: No allocation
- **WHEN** file operations and faults run under the allocator that aborts on allocation
- **THEN** the run completes

### Requirement: Power loss
The simulator SHALL offer a power loss that delivers no completion for any
operation in flight, frees every socket and file slot, and leaves each
write not yet synced whole, absent, or cut at a sector boundary, as the
seed decides.

#### Scenario: Torn write
- **WHEN** a write of several sectors is not synced and power is lost
- **THEN** the file holds the old bytes, the new bytes, or new bytes up to a sector boundary followed by old ones

#### Scenario: Overlapping writes
- **WHEN** two unsynced writes overlap and power is lost
- **THEN** the range holds a state that keeping or dropping each write in order could produce

#### Scenario: Operations in flight
- **WHEN** power is lost while a write and a receive are in flight
- **THEN** no completion is delivered for either and their slots are free

#### Scenario: Same seed
- **WHEN** the same run with a power loss is repeated with one seed
- **THEN** the files hold the same bytes both times

### Requirement: Disk faults by rate
The simulator SHALL inject, each at its own seeded rate, short writes, torn
and lost writes both reported and silent, writes to the wrong place, read
errors, flipped bits on read, failed syncs and refused reserves and
punches.

#### Scenario: Rates at zero
- **WHEN** every disk fault rate is zero
- **THEN** a run's trace hash equals that of the same run before this change

#### Scenario: Short write
- **WHEN** a write is cut short
- **THEN** it completes with a count below what was asked, on a sector boundary, and the file holds that many of its bytes

#### Scenario: Short write in a chain
- **WHEN** the write of a write-then-sync chain is cut short
- **THEN** the sync completes as cancelled

#### Scenario: Silent lost write
- **WHEN** a write is lost silently
- **THEN** it completes with its length and a later read returns the bytes that were there before

#### Scenario: Reported torn write
- **WHEN** a write is torn and reported
- **THEN** it completes with the I/O error and the file holds a prefix of it ending on a sector boundary

#### Scenario: Read error until rewritten
- **WHEN** a sector has a read error
- **THEN** reads that touch it complete with the I/O error until a write covers the sector, and succeed after

#### Scenario: Misdirected write
- **WHEN** a write is misdirected
- **THEN** it completes with its length, its bytes are at another sector-aligned offset of the same file, and the range it named is unchanged

#### Scenario: Flipped bit
- **WHEN** a read has a bit flipped
- **THEN** the buffer differs from the file in one bit and a second read without the fault returns the file's bytes

### Requirement: Failed sync
A sync that fails in the simulator SHALL complete with the I/O error, and
the writes it covered SHALL stay readable and never become durable.

#### Scenario: Retry after a failed sync
- **WHEN** a sync fails, a second sync of the same file succeeds, and power is lost
- **THEN** the writes made before the first sync are absent

#### Scenario: Read after a failed sync
- **WHEN** a sync fails and the file is read
- **THEN** the read returns the written bytes

### Requirement: Placed faults
The simulator SHALL let a test make the file operation with a given number,
counted at submission, fail in a given way, whatever the rates are, and
SHALL count the faults it injected by kind.

#### Scenario: Fault at an operation
- **WHEN** a test places a torn write at the fifth file operation
- **THEN** that operation is torn and no other operation is affected

#### Scenario: Fail after
- **WHEN** a test places "fail after" on a rename
- **THEN** the rename completes with the I/O error and the new name exists

#### Scenario: Write fault on another operation
- **WHEN** a test places a torn write on an operation that is a sync
- **THEN** the sync completes with the I/O error without effect

#### Scenario: Counted
- **WHEN** a run has injected two lost writes
- **THEN** the counter for lost writes reads two

### Requirement: One simulation for files and sockets
Disk faults, network faults and time SHALL come from one seed and one
virtual clock within a simulated machine.

#### Scenario: Replay
- **WHEN** a run with both network and disk faults is repeated with one seed
- **THEN** both runs deliver the same completions in the same order
