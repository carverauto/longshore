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
Header framing SHALL offer to receive a frame so that its body, not its
header, starts at a block boundary of the frame's memory, in every
receive mode, so that the body can go to a direct-I/O write from where it
lies.

#### Scenario: Body written as it lies
- **WHEN** a frame with a 256-byte header is received with the body asked for at a block, and its body is written to a file with direct I/O
- **THEN** the body starts at a multiple of the block size in its buffer, and no byte of it was moved after it was received

#### Scenario: Frame memory too small
- **WHEN** a plan's frame memory cannot hold the lead before the header and the largest frame
- **THEN** planning fails with an error naming the frame size

