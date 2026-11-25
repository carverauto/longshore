## ADDED Requirements
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
