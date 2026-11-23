# core-mailbox Specification

## Purpose
Bounded lock-free queues that carry fixed-size `Copy` messages from any
thread into an I/O thread, wake that thread through its completion queue,
and return answers through preallocated slots. The S3 client is built on
them.

## Requirements
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

#### Scenario: Receiver without a backend
- **WHEN** the receiver waits for a message by parking and a sender sends one
- **THEN** the receiver takes the message

### Requirement: Back pressure
A send to a full mailbox SHALL be refused and return the message, and the
queue SHALL allocate nothing after it is built.

#### Scenario: Full
- **WHEN** as many messages are queued as the capacity allows and another is sent
- **THEN** the send is refused and the queued messages are unchanged

#### Scenario: No allocation
- **WHEN** messages are sent and taken under the allocator that aborts on allocation
- **THEN** the run completes

### Requirement: Answers
The mailbox crate SHALL provide slots through which the receiving thread
answers a caller, who waits either by blocking or by a future, for any
answer type, and SHALL let a caller wait for a slot on several mailboxes at
once.

#### Scenario: Blocking caller
- **WHEN** a caller sends a request and waits by blocking
- **THEN** it returns with the answer the receiver gave

#### Scenario: Future
- **WHEN** a caller awaits the same request as a future
- **THEN** its waker is called when the receiver answers and the future yields the answer

#### Scenario: Caller gives up
- **WHEN** a caller drops its wait before the answer
- **THEN** the slot is freed when the receiver answers, the number the caller attached is handed back to the receiver, and no other caller is given that answer

#### Scenario: More waiters than slots
- **WHEN** one slot is in use and two callers wait for it
- **THEN** both wait, and each is given the slot in turn

#### Scenario: Waiting within the preallocated number
- **WHEN** no more callers wait than the number set at build, under the allocator that aborts on allocation
- **THEN** the run completes

### Requirement: Stopping
A sender SHALL be able to ask the receiver to stop. Messages queued before
the request SHALL be delivered and messages sent after it SHALL be refused
as closed.

#### Scenario: Queued before the stop
- **WHEN** a message is queued and a stop is then asked for
- **THEN** the receiver takes the message and then the stop

#### Scenario: Sent after the stop
- **WHEN** a message is sent after a stop was asked for
- **THEN** the send is refused as closed

#### Scenario: Stop when full
- **WHEN** the queue is at its capacity and a stop is asked for
- **THEN** the stop is accepted

### Requirement: Closing
When the receiver closes, every message SHALL be either taken by the
receiver or refused to its sender, and every caller whose request was sent
and not answered SHALL be given the closed error.

#### Scenario: Send racing a close
- **WHEN** a sender sends while the receiver closes, in any interleaving
- **THEN** the message is taken by the receiver or the send is refused, and never neither

#### Scenario: Receiver answers first
- **WHEN** the receiver answers one of two waiting callers and then closes
- **THEN** the first caller gets its answer and the second the closed error

#### Scenario: Send after close
- **WHEN** a message is sent after the receiver has closed
- **THEN** the send is refused as closed

### Requirement: Checked under the model
The queue, the slots, stopping and closing SHALL be checked by model tests
that explore the interleavings of their threads.

#### Scenario: Queue under the model
- **WHEN** two senders and the receiver run under the model
- **THEN** every message is taken exactly once in every interleaving explored

### Requirement: Replay in the simulator
Mailboxes between several simulated backends on one thread SHALL deliver
messages in an order fixed by the backends' seeds and the test's order of
ticks.

#### Scenario: Two cores in one simulation
- **WHEN** two simulated backends exchange messages and the run is repeated with the same seeds and order of ticks
- **THEN** both runs deliver the same messages in the same order

### Requirement: S3 client on the mailbox
The S3 client SHALL use the mailbox for its hand-off with no change to the
behaviour of its calls.

#### Scenario: Client tests
- **WHEN** the client's tests and its model tests run after the move, unedited
- **THEN** they pass

#### Scenario: Failed thread
- **WHEN** an I/O thread's backend fails while callers hold lent buffers
- **THEN** each caller is answered and the buffers' memory stays valid until the client is dropped

