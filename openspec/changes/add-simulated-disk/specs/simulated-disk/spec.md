## ADDED Requirements

### Requirement: Durable and written state
The simulator SHALL keep, for each file and directory, the state that has
been written and the state that is durable, and SHALL make writes durable
only when a sync of that file or directory completes with success.

#### Scenario: Synced write survives
- **WHEN** a write is synced and power is then lost
- **THEN** a read after the files are opened again returns the write

#### Scenario: Rename without a directory sync
- **WHEN** a file is renamed, its directory is not synced, and power is lost
- **THEN** the file may be found under its old name

### Requirement: Power loss
The simulator SHALL offer a power loss that delivers no completion for the
operations in flight and leaves each write not yet synced whole, absent, or
cut at a 512-byte boundary, as the seed decides.

#### Scenario: Torn write
- **WHEN** a write of several sectors is not synced and power is lost
- **THEN** the file holds the old bytes, the new bytes, or new bytes up to a 512-byte boundary followed by old ones

#### Scenario: Operations in flight
- **WHEN** power is lost while a write is in flight
- **THEN** no completion is delivered for it

#### Scenario: Same seed
- **WHEN** the same run with a power loss is repeated with one seed
- **THEN** the files hold the same bytes both times

### Requirement: Disk faults by rate
The simulator SHALL inject, each at its own seeded rate, short writes, torn
writes, lost writes, writes to the wrong place, read errors, flipped bits on
read, failed syncs and refused reserves and punches.

#### Scenario: Rates at zero
- **WHEN** every disk fault rate is zero
- **THEN** a run's trace hash equals that of the same run before this change

#### Scenario: Silent lost write
- **WHEN** a write is lost silently
- **THEN** it completes with its length and a later read returns the bytes that were there before

#### Scenario: Read error until rewritten
- **WHEN** a range has a read error
- **THEN** reads of it complete with the I/O error until a write covers it, and succeed after

#### Scenario: Misdirected write
- **WHEN** a write is misdirected
- **THEN** it completes with its length and its bytes are at another block-aligned offset of the same file

### Requirement: Failed sync
A sync that fails in the simulator SHALL complete with the I/O error and
discard the writes it would have made durable.

#### Scenario: Retry after a failed sync
- **WHEN** a sync fails and a second sync of the same file succeeds
- **THEN** the writes made before the first sync are not durable

### Requirement: Placed faults
The simulator SHALL let a test make the file operation with a given number
fail in a given way, whatever the rates are.

#### Scenario: Fault at an operation
- **WHEN** a test places a torn write at the fifth file operation
- **THEN** that operation is torn and no other operation is affected

### Requirement: One simulation for files and sockets
Disk faults, network faults and time SHALL come from one seed and one
virtual clock.

#### Scenario: Replay
- **WHEN** a run with both network and disk faults is repeated with one seed
- **THEN** both runs deliver the same completions in the same order
