## ADDED Requirements

### Requirement: Zero-copy sends not slower than copied sends
Zero-copy sends over a network card SHALL NOT, with default settings,
carry fewer bytes a second than copied sends of the same frames, for any
frame length at or above the zero-copy threshold, compared by the medians
of repeated runs.

#### Scenario: 64 KiB frames over a card
- **WHEN** one thread sends 64 KiB frames on four connections to another machine with default settings, with zero-copy sends and without
- **THEN** over five runs each, the median with zero-copy sends is no less than nine tenths of the median without

### Requirement: A frame's body at a block boundary
Header framing SHALL offer to receive a frame, in direct mode, so that its
body, not its header, starts at a block boundary of the frame's memory,
with and without TLS, so that the body can go to a direct-I/O write from
where it lies. Without TLS no byte of the body SHALL be moved after it is
received.

#### Scenario: Body written as it lies
- **WHEN** a frame with a 256-byte header is received in direct mode with the body asked for at a block of 4,096 bytes
- **THEN** the frame starts 3,840 bytes into its memory and its body at a multiple of the block

#### Scenario: Where it cannot be given
- **WHEN** a connection is opened with the option in ring mode, or with frame memory that cannot hold the lead and the largest frame or is not whole blocks
- **THEN** opening it fails
