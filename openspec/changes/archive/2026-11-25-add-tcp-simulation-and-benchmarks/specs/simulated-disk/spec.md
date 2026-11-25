## ADDED Requirements
### Requirement: Files opened for durable writes
The simulated disk SHALL make a completed write to a file opened for durable
writes durable before its completion is delivered, so that it is there and
whole after a power loss with no sync, and SHALL let that write fail as a
sync does at the rate of failed syncs. A write to a file not opened so SHALL
remain losable and tearable by a power loss until it is synced.

The model is stronger than a kernel in two ways, which the documentation
SHALL state: a durable write also makes durable whatever else of the same
file was written and not yet synced, where a kernel syncs the range written;
and a durable write is whole or absent after a power loss, never torn, where
real media can hold part of it.

#### Scenario: Durable without a sync
- **WHEN** records are written to a file opened for durable writes, whose directory entry was synced, and power is lost with no sync, over many seeds
- **THEN** every record is read back whole after the power loss

#### Scenario: The plain file can lose them
- **WHEN** the same records are written to a file not opened so and power is lost with no sync, over many seeds
- **THEN** in some runs records are lost and in some torn at a sector boundary, and the same seed gives the same outcome

#### Scenario: The name is not covered
- **WHEN** a durable write is made to a newly created file whose directory was never synced, and power is lost
- **THEN** the file is not there afterwards

#### Scenario: A failed sync fails the write
- **WHEN** every sync is set to fail and a durable write is made
- **THEN** the write completes with an I/O error, the failure is counted, and its data is absent after a power loss
