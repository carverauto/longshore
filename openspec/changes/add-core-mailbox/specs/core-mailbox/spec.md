## ADDED Requirements

### Requirement: Mailbox into an I/O thread
The mailbox SHALL carry fixed-size messages from any thread to the one
thread that owns an `Io`, in a queue whose capacity is fixed when it is
built, and SHALL wake that thread through its backend.

#### Scenario: Message while the receiver waits
- **WHEN** the receiver is waiting in `tick` and another thread sends a message
- **THEN** the receiver's `AwaitWake` completes and it takes the message

#### Scenario: Order from one sender
- **WHEN** one sender sends two messages
- **THEN** the receiver takes them in the order sent

#### Scenario: No timer
- **WHEN** a message is sent and the receiver has no timer pending
- **THEN** the receiver takes the message

### Requirement: Back pressure
A send to a full mailbox SHALL be refused and return the message, and the
mailbox SHALL allocate nothing after it is built.

#### Scenario: Full
- **WHEN** as many messages are queued as the capacity allows and another is sent
- **THEN** the send is refused and the queued messages are unchanged

#### Scenario: No allocation
- **WHEN** messages are sent and taken under the allocator that aborts on allocation
- **THEN** the run completes

### Requirement: Answers
The mailbox crate SHALL provide slots through which the receiving thread
answers a caller, who waits either by blocking or by a future, for any
answer type.

#### Scenario: Blocking caller
- **WHEN** a caller sends a request and waits by blocking
- **THEN** it returns with the answer the receiver gave

#### Scenario: Future
- **WHEN** a caller awaits the same request as a future
- **THEN** its waker is called when the receiver answers and the future yields the answer

#### Scenario: Caller gives up
- **WHEN** a caller drops its wait before the answer
- **THEN** the slot is freed when the receiver answers and no other caller is given that answer

#### Scenario: No slot free
- **WHEN** every slot is in use and the waiters for one are as many as allowed
- **THEN** a further caller is told so and nothing grows

### Requirement: Closing
When the receiving thread goes away, every later send SHALL be refused and
every caller waiting for an answer SHALL be given the closed error.

#### Scenario: Receiver stops
- **WHEN** the receiver stops while two callers wait for answers
- **THEN** both are given the closed error

#### Scenario: Send after close
- **WHEN** a message is sent after the receiver has stopped
- **THEN** the send is refused as closed

### Requirement: Checked under the model
The queue, the slots and the closing SHALL be checked by model tests that
explore the interleavings of their threads.

#### Scenario: Queue under the model
- **WHEN** two senders and the receiver run under the model
- **THEN** every message is taken exactly once in every interleaving explored

### Requirement: Replay in the simulator
Mailboxes between several simulated backends on one thread SHALL deliver
messages in an order fixed by the test's seed.

#### Scenario: Two cores in one simulation
- **WHEN** two simulated backends exchange messages and the run is repeated with one seed
- **THEN** both runs deliver the same messages in the same order

### Requirement: S3 client on the mailbox
The S3 client SHALL use the mailbox for its hand-off with no change to its
API.

#### Scenario: Client tests
- **WHEN** the client's tests and its model tests run after the move
- **THEN** they pass without change to their expectations
