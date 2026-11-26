## ADDED Requirements

### Requirement: Zero-copy sends not slower than copied sends
Zero-copy sends over a network card SHALL NOT, with default settings,
carry fewer bytes a second than copied sends of the same frames, for any
frame length at or above the zero-copy threshold, compared by the medians
of repeated runs.

#### Scenario: 64 KiB frames over a card
- **WHEN** one thread sends 64 KiB frames on four connections to another machine with default settings, with zero-copy sends and without
- **THEN** over five runs each, the median with zero-copy sends is no less than nine tenths of the median without
