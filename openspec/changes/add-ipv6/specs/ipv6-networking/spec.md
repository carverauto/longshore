## ADDED Requirements

### Requirement: IPv6 connections
The client SHALL connect to a store over IPv6 as well as IPv4.

#### Scenario: Fixed IPv6 endpoint
- **WHEN** the endpoint is configured as an IPv6 address and port
- **THEN** operations complete over that address

### Requirement: AAAA resolution with a family preference
The resolver SHALL query AAAA records as well as A records and SHALL choose
between families by a configured preference.

#### Scenario: IPv6-only network
- **WHEN** a name has only AAAA records
- **THEN** the client connects over IPv6

#### Scenario: Preferred family unreachable
- **WHEN** a name has both record types and connections to the preferred family fail
- **THEN** the client connects over the other family and the operation succeeds
