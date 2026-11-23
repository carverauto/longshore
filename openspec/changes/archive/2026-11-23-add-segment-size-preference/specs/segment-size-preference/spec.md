## ADDED Requirements

### Requirement: Segment size of a connection
The I/O backend SHALL report the maximum segment size of a connection it
opens, when configured to measure it, on every supported kernel.

#### Scenario: Measured connect
- **WHEN** measuring is on and a connect succeeds
- **THEN** its completion carries the connection's maximum segment size

#### Scenario: Failed connect
- **WHEN** measuring is on and a connect fails
- **THEN** the socket slot is free and the process holds no descriptor for it

### Requirement: Preference for large-segment addresses
The resolver SHALL, when the setting is on, hand out the addresses of a
host whose connections have large segments in preference to those with
small ones.

#### Scenario: Mixed addresses
- **WHEN** a host has addresses with large and with small segments and each has been connected to once
- **THEN** later lookups return only the large-segment addresses

#### Scenario: Too few large-segment addresses
- **WHEN** fewer addresses have large segments than the configured minimum
- **THEN** lookups also return the best of the others, up to that minimum

#### Scenario: Setting off
- **WHEN** the setting is off
- **THEN** lookups rotate through every address as before
