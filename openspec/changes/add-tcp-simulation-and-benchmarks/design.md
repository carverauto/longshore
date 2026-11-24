## Context
`add-tcp-crate` built `longshore_tcp` and checked it with hand-written
scenarios on the simulator, the portable backend and a real kernel. Its
review then found about two dozen faults in nine rounds. Almost none was
in a path a scenario described; nearly all were an awkward order of events
or a pool that was empty at an awkward moment:

| Kind of fault | Examples from the review | Share |
|---|---|---|
| A close racing with something in flight | an accept slot reused while a zero-copy send's notification was pending; a connect that succeeded after its close was submitted; `Readable` queued after `close`; a redial after `close` on a connection that was down | about a third |
| A pool empty at the wrong moment | frame memory exhausted when the stream ended; a pause never lifted; no `Writable` when the shared segment pool, not the queue, was full; the hold counter at its limit | about a third |
| An end of stream with input not yet processed | a partial frame reported as an orderly end; a header-only frame stalled; TLS records and a close alert arriving with the FIN | about a fifth |
| The TLS ring path | a carried record left unopened; a receive buffer overrun after the handshake | the rest |

The causes were upstream of the code:
1. The scenarios were all "when X, then Y" on a quiet system. None said
   "while a send is in flight", "with no frame memory free" or "in the
   same turn as the end of the stream".
2. The design described behaviour in prose. A connection has eight states
   and some twenty flags, and nobody wrote down what each event means in
   each of them.
3. No invariant was stated before a finding forced the question.
4. No place that takes from a pool had a stated answer for "empty".
5. The tests that find such faults, random operations under pressure, were
   put off to this change.
6. The review came once, at the end of 5,500 lines.

This change therefore starts with what should have come first, and both
remaining changes are written to the same method.

## Goals / Non-Goals
- Goals: the state table and the invariants below made the checked
  contract of `Net`; a model test that drives random operations against
  them, so that a fault of the four kinds above is found by a seed and not
  by a reviewer; a conformance suite every transport must pass (change 3
  runs it); the fault campaigns, the packet device, properties, fuzzing and
  the benchmarks of the proposal.
- Non-Goals: new features of `Net`. A cell of the table that the code gets
  wrong is fixed here as a bug.

## Method
These rules apply to this change and to `add-tcp-af-xdp-backend`.

1. **Table before code.** Every state machine has a table in its design:
   states down, events across, every cell filled, "cannot happen" and
   "ignored" included with the reason. Code for a machine is not started
   before its table is reviewed.
2. **Invariants before code**, each written so that a test can check it
   after every step.
3. **An answer for every empty pool**: what the taker does, and what wakes
   it. No taker may drop data or wait for a wake nobody sends.
4. **Adversarial scenarios.** Each requirement in a spec delta has, besides
   its plain scenario, the ones that apply of: under close, under
   exhaustion, at the end of the stream, with completions in another order.
5. **A model test with the first code**, not after it: random operations
   and faults by seed, small pools, the invariants checked after every
   step, a failing seed replayable by one command.
6. **Review per task group.** Each numbered group of `tasks.md` ends with a
   review of that group's diff (no-mistakes, or `/code-review`), and its
   findings are fixed before the next group starts.
7. **One state, not many flags.** Where a table shows that a combination
   of flags cannot occur, the code is changed so that it cannot be written.

## The connection state table
Rows are the states of `net::state::St`. Columns are everything that can
reach a connection: calls of the caller, what the transport reports, and
deadlines. The table was checked cell by cell against the code as merged
(`net/conn.rs`, `listen.rs`, `rx.rs`, `pull.rs`, `tx.rs`, `secure.rs`);
where the code does something else than a sound contract would, the cell
says what the code does and the list after the tables says what is wrong.

"—" is "cannot happen", with the reason in the notes. `free?` stands for
"clear the in-flight flag this report belongs to, then free the slot if it
is `Closing`, its close has completed and nothing else is in flight". A
slot is freed in no other way. An event is owed only to a connection that
is `Up`, `Refused` or `Dying`; in every other state nothing is queued.

**Calls of the caller**

| State | `close()` | `shutdown_write()` | a send | `next_frame()` |
|---|---|---|---|---|
| `Free` | `Stale` (1) | `Stale` | `Stale` | `Stale` |
| `Connecting` | handle retired, → `Closing`, the close is submitted at once; no event | `Stale` (2) | `NotUp` | none |
| `Securing` | — (3) | — (3) | — (3) | — (3) |
| `Up` | as `Connecting` | marked; the shutdown goes out when the queue is empty and no send is in flight; once | queued; `Full` when the queue or the segment pool is full; `NotUp` after `shutdown_write` | frames in order; none when nothing can be cut yet |
| `Refused` | as `Connecting` | `Stale` (2) | queued (4) | none: it does not read |
| `Dying` | as `Connecting` | `Stale` (2) | `NotUp` (5) | frames received before the end; when input is left that cannot be delivered yet, the end is held back (6) |
| `Closing` | after `Down`: the redial is cancelled and the handle retired. Otherwise `Stale` | `Stale` | `Stale` or `NotUp` | none |
| `Backoff` | handle retired, deadline cleared, → `Free` | `Stale` (2) | `NotUp` | none |

**Reports of the transport**

| State | Connected | Secured | data | end of stream | receive ended otherwise | Sent | ShutDown | Closed |
|---|---|---|---|---|---|---|---|---|
| `Free` | — (7) | — (7) | — (7) | — (7) | — (7) | — (7) | — (7) | — (7) |
| `Connecting` | ok, plain: → `Up`, `Up` owed, reading starts. ok, TLS: the handshake starts; stays `Connecting`. failed: → `Dying` (`Reset`) | ok: → `Up`, `Up` owed; in ring mode what arrived behind the handshake is opened. failed: → `Dying` (`Tls`, or `Reset`) | — (8) | — (8) | — (8) | — (8) | — (8) | — (9) |
| `Securing` | — (10) | ok: admitted (→ `Up` and `Accepted` owed; at the limit → `Refused` and `Refused` owed; listener gone → `Closing`). failed: handle retired, → `Closing`, no event | — (8) | — (8) | — (8) | — (8) | — (8) | — (9) |
| `Up` | — (11) | — (11) | ring: queued, `Readable` owed once; TLS ring: opened into frame memory first. direct: the header, then the frame; `Readable` owed when a frame is whole | plain: → `Dying` (`Orderly`) (12). TLS ring: stays `Up` until every record received is opened, then `Orderly` if a close alert was among them, else `Reset`. TLS direct: `Reset` unless a close alert came | ring empty or receive cancelled: armed again on the next `poll`. An error: → `Dying` (`Reset` or `Tls`) | ok: the span advances; the next goes out; `Writable` owed if a send was refused. failed: → `Dying` | cleared | — (9) |
| `Refused` | — (11) | — (11) | — (13) | — (13) | — (13) | ok: when the queue is empty the handle is retired and → `Closing`, no event. failed: the same | — (2) | — (9) |
| `Dying` | `free?` (no-op: not closing yet) | `free?` | the ring buffer goes back; nothing is kept | ignore | ignore | ignore; what is queued is dropped when the slot is freed (5) | cleared | — (9) |
| `Closing` | `free?` (14) | `free?` | the ring buffer goes back; `free?` | `free?` | `free?` | `free?` | `free?` | the close has completed; `free?`; then → `Free`, or → `Backoff` for a redial |
| `Backoff` | — (7) | — (7) | — (7) | — (7) | — (7) | — (7) | — (7) | — (7) |

**Deadlines, events pulled, and the kernel's accept**

| State | deadline fires | the caller pulls the end event | the kernel accepts a connection into the slot |
|---|---|---|---|
| `Free` | — (none armed) | — | the connection takes the slot: TLS listener → `Securing` with the handshake deadline; otherwise admitted as in "Secured ok" |
| `Connecting` | connect timeout: → `Dying` (`Timeout`); the close that follows ends the connect or the handshake | — | — (15) |
| `Securing` | handshake timeout: handle retired, → `Closing`, no event | — | — (15) |
| `Up` | idle timeout, if nothing was sent or received for that long: → `Dying` (`Timeout`); else armed again | — | — (15) |
| `Refused` | — (none armed) | — | — (15) |
| `Dying` | only while the end is held back (6): the input is dropped and the end owed as `Timeout` | `Closed`: handle retired, → `Closing`. `Down`: handle kept, → `Closing` for a redial | — (15) |
| `Closing` | — (none armed) | — | the connection is parked: nothing is started for it, and it takes the slot when the slot is freed (16) |
| `Backoff` | → `Connecting`, same handle | — | — (17) |

Notes:
1. A handle whose connection has gone names nothing.
2. `shutdown_write` answers `Stale` for a valid handle whose connection is
   not `Up`. The name is wrong for that case (finding C5).
3. The caller has no handle to a connection in `Securing`.
4. The contract is one frame; the code does not count them (finding C4).
   The connection closes, without an event, on the first `poll` after the
   `Refused` event was pulled on which its queue is empty: a caller that
   means to send its frame queues it in the turn it pulls the event.
5. The end of the peer's stream ends the connection in both directions:
   from `Dying` on nothing is sent, and what is queued is dropped (finding
   C1).
6. The end event is withdrawn when `next_frame` returns nothing while
   input is left (no frame memory, or no hold left on the backlog), and
   offered again, behind a `Readable`, on every `poll`. The idle timeout,
   counted from the first time, bounds the wait; without one nothing does
   but `close()` (finding C3). It works only if the caller pulls frames
   before it pulls the next event (finding C2).
7. Nothing is in flight for a `Free` or `Backoff` slot: a slot gets there
   only through `free?` (I3).
8. No receive, send or shutdown is started before the connection is `Up`
   or `Refused`. A handshake's own operations end in `Secured`.
9. `Closed` is reported for a close `Net` asked for, and `Net` asks only
   on the way into `Closing`.
10. An accepted connection is connected when it is reported.
11. A connect and a handshake are over before `Up` and `Refused`.
12. The reason is settled when the end event is pulled: `Reset` if what
    was pulled by then leaves a frame unfinished, `Protocol` if pulling
    found bytes that are no frame.
13. A refused connection does not read.
14. A connect that succeeds after its close was submitted leaves no
    socket: the close took it. `Net` relies on the backend for that
    (assumption A1 of the conformance suite).
15. The kernel fills only a socket slot that is empty, and `Net` empties
    one only by a close it submits on the way into `Closing`.
16. This is the fix for the slot being overwritten while a zero-copy
    send's notification was still due.
17. `Backoff` is a state of connections the caller opened; their slots
    are not in the accept range.

## The listener state table

| State | `listen()` | `close_listener()`, `drain()` | accept ok | accept failed | Closed | deadline |
|---|---|---|---|---|---|---|
| `Free`, no failure owed | takes the slot. Bind ok: → `Listening`, an accept starts. In use, with a retry time: → `Binding`. Otherwise the error | `Stale` | the connection is closed at once, unannounced (a) | the reserved slot goes back; nothing else (a) | — | — |
| `Free`, failure owed | not picked until `ListenFailed` has been pulled; pulling it retires the handle | the failure is forgotten | as above | as above | — | — |
| `Binding` | — | handle retired, → `Free`, nothing owed | — (b) | — (b) | — | bind again. Ok: → `Listening`. Still in use, time left: again in 50 ms. Otherwise: → `Free`, `ListenFailed` owed |
| `Listening` | — | handle retired, → `Closing`, the close is submitted | the connection takes its slot, or is parked if the slot is still closing; the wait after failures is reset; the next accept starts | cancelled: nothing. Otherwise wait (5 ms, doubling to 1 s; the length is kept until an accept works), then accept again | — | the wait is over: accept again; `poll` retries if the driver has no room |
| `Closing` | — | `Stale` | the connection is closed at once, unannounced | the reserved slot goes back | → `Free` (c) | —; every `poll` retries a close the driver had no room for |

a. A report can arrive for a listener that is already `Free`: its accept's
   last report may come after its close's (finding C6).
b. No accept is started before the bind.
c. The code frees the slot when the close completes, without waiting for
   the accept's last report (finding C6).

## The TLS phase table of the kernel transport
Per connection slot, in `kernel/secure.rs`. A phase other than `Plain`
routes the slot's send and receive completions through its stream.

| Phase | `secure` | a send or receive completion | `shutdown_write` | `close` submitted | Closed |
|---|---|---|---|---|---|
| `Plain` | the stream is reset, the handshake starts: → `Handshake` | reported as it is | a shutdown | — | — |
| `Handshake` | — | fed to the handshake. Pending: nothing reported. Done: → `Secure`, `Secured` ok. Failed: → `Plain`, `Secured` failed | — (not up yet) | → `Aborted` | stays (the operation is still due) |
| `Aborted` | — | → `Plain`, `Secured` reported as cancelled | — | stays | stays |
| `Secure` | — | send: fed to the stream; `Sent` when all of it has gone. receive: `Received` with plaintext; an end without a close alert is a reset | the close alert is sent: → `Alerting` | stays | → `Plain`, keys forgotten |
| `Alerting` | — | send: when the alert has gone, → `Secure` and the shutdown is submitted (`ShutDown` at once if the driver has no room). receive: as `Secure` | — (once) | → `AlertAborted` | stays |
| `AlertAborted` | — | send: → `Plain`, `ShutDown` reported as cancelled. receive: reported as it is | — | stays | stays |

## Where the code falls short of the tables
Found by checking the tables against the code. None is fixed yet; each is
a decision or a fault for group 1, and the model test must reach it.

- **C1 Half-close by the peer ends everything.** A peer that sends a
  request and then shuts its write side gets no answer: the connection is
  `Dying` at the end of its stream, sends are refused and the queue is
  dropped. Request-then-shutdown-then-read is a common pattern. Decision
  needed: support it (a state between `Up` and `Dying` in which sending
  goes on until the caller shuts down or closes), or state in the spec
  that the end of either stream ends the connection.
- **C2 Frames are lost if the end event is pulled before them.** Pulling
  `Closed` retires the handle; frames received before the end and not yet
  pulled are then unreachable. `Readable` always comes first, so a caller
  that pulls frames at each `Readable` loses nothing, but a caller that
  drains all events before pulling frames does. Either the end event must
  not be offered while input is left, or the contract must say so.
- **C3 A held-back end has no bound without an idle timeout.**
- **C4 A refused connection can be sent any number of frames.**
- **C5 `shutdown_write` answers `Stale` for a valid handle that is not
  up**; `NetError` lacks a "not up".
- **C6 A listener slot can be taken again before its accept has made its
  last report.** That report then clears the new listener's accept flag
  (a second accept is started while one runs), or hands the new listener
  a connection its predecessor accepted. This is the listener's twin of
  the accept-slot fault fixed for connections; the fix is the same: a
  listener slot is `Free` only when its close has completed and no accept
  is in flight.
- **C7 The connect timeout marks the socket open whether or not there is
  one**, and relies on the backend reporting the close of an empty socket
  slot like any other (assumption A2 of the conformance suite).

## Invariants
Checked after every step of the model test, and by debug assertions where
that is cheap.

- **I1 Slot ownership.** A connection slot is owned by at most one
  connection; it is freed only when its close has completed and nothing is
  in flight for it.
- **I2 One in flight.** Per connection at most one send, one receive (or
  one armed receive), and one of connect, handshake, shutdown and close.
  Per listener at most one accept and one close.
- **I3 Quiet when free.** Nothing is in flight, queued or held for a slot
  that is `Free` or `Backoff`.
- **I4 Handles.** A retired handle reaches nothing. A handle the caller
  still holds never comes to name another connection or listener.
- **I5 Events.** No event for a retired handle. `Closed` is the last event
  of a connection. `Up` and `Down` alternate. `Accepted` or `Refused`
  comes at most once. An event is owed only in `Up`, `Refused` and
  `Dying`.
- **I6 Delivery.** Bytes are delivered exactly once, in order. Everything
  received before the end of the stream can be pulled before the end
  event is offered, whatever the pools hold at that moment (the code
  meets this only for a caller that pulls frames at each `Readable`:
  finding C2).
- **I7 The end.** `Orderly` only if the stream ended between frames (with
  TLS: after a close alert). Otherwise `Reset`, `Tls`, `Protocol` or
  `Timeout`.
- **I8 Memory.** Every extent, segment and ring buffer has exactly one of:
  free, or held with a count that equals its holders. Counts never go
  below zero or wrap. With no connection open and no frame held, every
  pool is full.
- **I9 Deadlines.** A slot has a deadline only in a state whose row has
  one, and `next_deadline` is the earliest of them.
- **I10 Wakes.** Whoever was refused for lack of room (`Full`, a paused
  read, an accept that waits) is told, or resumed, once there is room,
  without any further call of the caller's being needed.
- **I11 Progress.** If the caller pulls every event and frame and releases
  what it holds, and the peer is not silent, the run does not stall.
- **I12 Allocation.** Nothing allocates after build, the TLS handshake
  aside.

## Exhaustion

| Pool | Who takes | When it is empty | What wakes the taker |
|---|---|---|---|
| Connection slots (own) | `connect`, a single accept | `NoSlots`; the accept is not started | a freed slot; `poll` starts accepts |
| Accept range (kernel's) | an armed accept | the kernel drops the connection, the accept fails, `Net` waits | the wait's deadline |
| Frame memory (extents) | direct receive, assembling a frame, a spill, TLS opening, `hold` | the read pauses, the input stays where it is, an end of stream waits; `hold` returns `NoMemory` | `release`, a send or write that drops its hold; `poll` retries |
| Hold count of one extent | a frame lent from a backlog, `send_lent`, a file write | the frame is not lent and stays; `Stale` or `Full` to the caller | a hold released |
| Send segments | `send_frame`, `send_bytes` | `Full`; the connection joins the fixed waiter list | a segment returned: one waiter is told `Writable`, and a waiter that leaves passes it on |
| Send queue of a connection | every send | `Full` | its own send completing: `Writable` |
| Buffer ring | the kernel, for armed receives | the receive ends with "no buffers" | `poll` arms again after giving buffers back; unpulled input is moved out at each `poll` |
| The driver's operation slots | every submit | the transport refuses as busy; `Net` keeps the work | the next `poll` |
| Event marks | every event | cannot be empty: one mark per connection | — |

## The model test
One test binary, `tcp_model`, on the simulator.

- **The adversary.** A seeded generator picks the next step among:
  connect, listen, close, close a listener, shutdown, send (every form and
  length, zero included), pull events, pull frames (all, some, none), hold,
  release, lend to another connection, advance time, turn the backend,
  restart a peer, and drain. Steps are chosen blind to state, so stale
  handles and calls in the wrong state are exercised.
- **Pressure.** Pools as small as the configuration allows: two frame
  buffers, two segments, two ring buffers, one accept slot, the driver
  with the fewest operation slots that are correct.
- **Order.** The simulator delivers completions of one tick in a seeded
  order (a new fault knob), so that a close's completion comes before,
  between and after those of the operations it ends.
- **The reference.** Per connection a plain byte queue per direction, and
  the life cycle allowed by the state table. After every step the test
  compares what was delivered with the queue and checks I1 to I12.
- **Replay.** A failure prints its seed and the step count;
  `tcp_model --seed N --steps M` replays it, and `--shrink` drops steps
  that are not needed to fail.
- **Budget.** Ten thousand steps over a few hundred seeds in
  `cargo test`; one hundred thousand seeds in the nightly workflow.

## The transport conformance suite
What `Net` relies on from any transport, as a suite a transport runs
against itself (`longshore_tcp::check::conformance`), so that change 3's
transport is held to it from its first commit:

- For a slot, `Closed` is reported exactly once per close asked for, and
  every operation in flight on the slot is reported (as ended) at some
  point, before or after it. Nothing is reported for a slot after both.
- A transport that fills slots itself (the accept range) never reports a
  connection in a slot for which it still owes a report.
- A send is reported once, with the bytes taken; the transport no longer
  reads its buffer after the report.
- A receive that ended at once is not also reported.
- `Secured` is reported once per `secure`, also when the slot is closed
  under it.
- A refusal as busy changes nothing.
- A1: a connect that would have succeeded after a close of its slot was
  submitted leaves no socket behind.
- A2: a close of a slot that holds no socket is reported like any other.

## Decisions
- **The tables describe `Net` as built**, checked cell by cell against the
  merged code, with the findings C1 to C7 listed where the code falls
  short. Where the model test finds the code disagreeing with a cell, the
  cell wins unless this document is changed first. C1 and C2 are decisions
  to take before group 1 starts.
- **Flags to states.** `sock_open`, `closed`, `redial` and the in-flight
  flags of a `Closing` connection become one small enum of what the slot
  still waits for; `paused`, `unopened` and `cut_off` become one enum of
  why a connection does not read. Done in group 1, behind the model test.
- **Benchmarks** as in the proposal; unchanged by this revision.

## Risks / Trade-offs
- The model test can be wrong where the code is right → the reference is
  kept to a byte queue and the two tables, nothing cleverer.
- Seeded completion order in the simulator changes existing trace hashes →
  the knob is off unless a test asks for it.

## Open Questions
- Whether the model's step generator should be shared with change 3's
  packet-level tests, or kept apart.
