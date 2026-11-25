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
2. The design described behaviour in prose. A connection had eight states
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

## Two decisions that change the machine
Checking the tables against the code (below) found two things the code
does that a caller cannot live with. Both are decided, and the tables
describe the machine **as it is to be**, not as merged; group 0 of the
tasks builds it.

**The end is held back until the input is pulled.** Today the end event
is owed the moment the stream ends, and pulling it retires the handle, so
frames received before the end are lost to a caller that pulls events
before frames. From now on an end is owed only when nothing received is
left to pull. A connection whose stream has ended with input left is
`Finishing`: `Readable` is owed, the end is not. When the caller has
pulled the last frame the end is settled, with its reason, and only then
owed. This also makes the reason exact (`Orderly` only if the stream ended
between frames), and replaces the special case that withdrew an end
already owed when frame memory ran out.

**Half-close.** Today the end of the peer's stream ends the connection in
both directions: a peer that sends a request, shuts its write side down
and waits for the answer gets none. With `ConnOpts::with_half_close(true)`
a connection whose peer has finished in order stays open for sending:
`Event::Finished(conn)` is owed once the last frame has been pulled, the
connection is `HalfClosed`, and it ends when the caller has shut its own
side down (`Closed` with `Orderly`), closes it, or a send fails. Without
the option (the default) the peer's end ends the connection as before, so
a caller that never answers after the end has nothing to do. The option
is per connection, and a listener sets it for what it accepts.

`shutdown_write` and sends on a connection that is not open for sending
answer `NotUp` (a new `NetError::NotUp`; `Stale` is for handles only).

## The connection state table
Rows are the states of `net::state::St`, with `Finishing` and `HalfClosed`
new. Columns are everything that can reach a connection: calls of the
caller, what the transport reports, and deadlines. Every cell that exists
today was checked against the merged code (`net/conn.rs`, `listen.rs`,
`rx.rs`, `pull.rs`, `tx.rs`, `secure.rs`).

"—" is "cannot happen", with the reason in the notes. `free?` stands for
"clear the in-flight flag this report belongs to, then free the slot if it
is `Closing`, its close has completed and nothing else is in flight". A
slot is freed in no other way. An event is owed only to a connection that
is `Up`, `Refused`, `Finishing`, `HalfClosed` or `Dying`.

"Open for sending" below means: `Up`; or `HalfClosed`; or `Finishing` with
half-close on and no failure so far; and in each case `shutdown_write`
not yet called.

**Calls of the caller**

| State | `close()` | `shutdown_write()` | a send | `next_frame()` |
|---|---|---|---|---|
| `Free` | `Stale` (1) | `Stale` | `Stale` | `Stale` |
| `Connecting` | handle retired, → `Closing`, the close is submitted at once; no event | `NotUp` | `NotUp` | none |
| `Securing` | — (2) | — (2) | — (2) | — (2) |
| `Up` | as `Connecting` | marked; the shutdown goes out when the queue is empty and no send is in flight; a second call does nothing | queued; `Full` when the queue or the segment pool is full; `NotUp` after `shutdown_write` | frames in order; none when nothing can be cut yet |
| `Refused` | as `Connecting` | `NotUp` | one frame is queued; a second is `NotUp` (3) | none: it does not read |
| `Finishing` | as `Connecting` | open for sending: as `Up`; else `NotUp` | open for sending: as `Up`; else `NotUp` | frames received before the end, in order. Pulling the last settles the end (4). Input that cannot be delivered yet (no frame memory, no hold left) stays, and so does the state |
| `HalfClosed` | as `Connecting` | as `Up`; when the shutdown has completed: → `Dying` (`Orderly`) | as `Up` | none: everything was pulled |
| `Dying` | as `Connecting` | `NotUp` | `NotUp` | none: everything was pulled |
| `Closing` | after `Down`: the redial is cancelled and the handle retired. Otherwise `Stale` | `Stale`, or `NotUp` after `Down` | `Stale`, or `NotUp` after `Down` | none |
| `Backoff` | handle retired, deadline cleared, → `Free` | `NotUp` | `NotUp` | none |

**Reports of the transport**

| State | Connected | Secured | data | end of stream | receive ended otherwise | Sent | ShutDown | Closed |
|---|---|---|---|---|---|---|---|---|
| `Free` | — (5) | — (5) | — (5) | — (5) | — (5) | — (5) | — (5) | — (5) |
| `Connecting` | ok, plain: → `Up`, `Up` owed, reading starts. ok, TLS: the handshake starts; stays `Connecting`. failed: → `Dying` (`Reset`) | ok: → `Up`, `Up` owed; in ring mode what arrived behind the handshake is opened. failed: → `Dying` (`Tls`, or `Reset`) | — (6) | — (6) | — (6) | — (6) | — (6) | — (7) |
| `Securing` | — (8) | ok: admitted (→ `Up` and `Accepted` owed; at the limit → `Refused` and `Refused` owed; listener gone → `Closing`). failed: handle retired, → `Closing`, no event | — (6) | — (6) | — (6) | — (6) | — (6) | — (7) |
| `Up` | — (9) | — (9) | ring: queued, `Readable` owed once; TLS ring: opened into frame memory first; after the armed receive has ended, the rest of a frame being put together comes straight into its memory (C10). direct: the header, then the frame; `Readable` owed when a frame is whole | → `Finishing`, or settled at once when no input is left (4). TLS ring: every record received is opened first; the end is in order only if a close alert was among them. TLS direct: in order only after a close alert | ring empty or receive cancelled: armed again on the next `poll`. An error: → `Finishing` with that failure, or → `Dying` when no input is left | ok: the span advances; the next goes out; `Writable` owed if a send was refused. failed: as a receive error | cleared; receiving goes on | — (7) |
| `Refused` | — (9) | — (9) | — (10) | — (10) | — (10) | ok or failed: the handle is retired, → `Closing`, no event | — (11) | — (7) |
| `Finishing` | — (9) | — (9) | the ring buffer goes back (12) | ignore (12) | ignore | open for sending: as `Up`, and a failure becomes the reason of the end. Otherwise ignore | cleared | — (7) |
| `HalfClosed` | — (9) | — (9) | — (12) | — (12) | — (12) | ok: as `Up`. failed: → `Dying` (`Reset`) | cleared: both sides are done, → `Dying` (`Orderly`) | — (7) |
| `Dying` | `free?` (not closing yet: nothing) | `free?` | the ring buffer goes back | ignore | ignore | ignore; what is queued is dropped when the slot is freed | cleared | — (7) |
| `Closing` | `free?` (13) | `free?` | the ring buffer goes back; `free?` | `free?` | `free?` | `free?` | `free?` | the close has completed; `free?`; then → `Free`, or → `Backoff` for a redial |
| `Backoff` | — (5) | — (5) | — (5) | — (5) | — (5) | — (5) | — (5) | — (5) |

**Deadlines, events pulled, and the kernel's accept**

| State | deadline fires | the caller pulls an event | the kernel accepts a connection into the slot |
|---|---|---|---|
| `Free` | — (none armed) | — | the connection takes the slot: TLS listener → `Securing` with the handshake deadline; otherwise admitted as in "Secured ok" |
| `Connecting` | connect timeout: → `Dying` (`Timeout`); the close that follows ends the connect or the handshake | — | — (14) |
| `Securing` | handshake timeout: handle retired, → `Closing`, no event | — | — (14) |
| `Up` | idle timeout, if nothing was sent or received for that long: → `Finishing` or `Dying` (`Timeout`); else armed again | `Up`, `Accepted`, `Readable`, `Writable`: no change | — (14) |
| `Refused` | — (none armed) | `Refused`: from the next `poll` on it closes when its queue is empty | — (14) |
| `Finishing` | idle timeout, counted from the end of the stream, if one is set: the input left is dropped, → `Dying` (`Timeout`) (15) | `Readable`: no change | — (14) |
| `HalfClosed` | idle timeout as `Up`: → `Dying` (`Timeout`) | `Finished`, `Writable`: no change | — (14) |
| `Dying` | — (none armed) | `Closed`: handle retired, → `Closing`. `Down`: handle kept, → `Closing` for a redial | — (14) |
| `Closing` | — (none armed) | — (none owed) | the connection is parked: nothing is started for it, and it takes the slot when the slot is freed (16) |
| `Backoff` | → `Connecting`, same handle | — | — (17) |

Notes:
1. A handle whose connection has gone names nothing.
2. The caller has no handle to a connection in `Securing`.
3. A refused connection may be sent one frame. It closes, without an
   event, on the first `poll` after the `Refused` event was pulled on
   which its queue is empty: a caller that means to send its frame queues
   it in the turn it pulls the event.
4. Settling the end, when the stream has ended and nothing received is
   left to pull:
   - the bytes pulled leave a frame unfinished: → `Dying` (`Reset`);
   - pulling found bytes that are no frame: → `Dying` (`Protocol`), at
     once, what is left is dropped;
   - a failure was recorded (a reset, a TLS failure, a failed send):
     → `Dying` with it;
   - in order, half-close on, and the caller has not shut its side down
     with the shutdown completed: → `HalfClosed`, `Finished` owed;
   - in order otherwise: → `Dying` (`Orderly`).
   `Dying` owes `Closed`, or `Down` for a reconnecting connection.
5. Nothing is in flight for a `Free` or `Backoff` slot: a slot gets there
   only through `free?` (I3).
6. No receive, send or shutdown is started before the connection is `Up`
   or `Refused`. A handshake's own operations end in `Secured`.
7. `Closed` is reported for a close `Net` asked for, and `Net` asks only
   on the way into `Closing`.
8. An accepted connection is connected when it is reported.
9. A connect and a handshake are over before any later state.
10. A refused connection does not read.
11. `shutdown_write` is refused for a refused connection.
12. Nothing is received after the end of the stream. But a connection
    is also `Finishing` when something failed (a send, say) with input
    left and its receive still running: what that receive then reports,
    bytes or the end, is of no use any more. A ring buffer goes back
    unread. (The model test's coverage showed these cells are reached;
    the table first had them as impossible.) Over TLS the peer's
    end is its close alert, and the receive runs on behind it: the end
    of the stream that follows, or a reset, reaches a `HalfClosed`
    connection and changes nothing.
13. A connect that succeeds after its close was submitted leaves no
    socket: the close took it. `Net` relies on the backend for that
    (assumption A1 of the conformance suite).
14. The kernel fills only a socket slot that is empty, and `Net` empties
    one only by a close it submits on the way into `Closing`.
15. Without an idle timeout a `Finishing` connection waits for its caller
    as long as an `Up` one would: not pulling is the caller's
    backpressure, and `close()` ends it. What it keeps meanwhile is frame
    memory, which is the caller's budget; its ring buffers are emptied
    into frame memory at each `poll` like anyone's.
16. This is the fix for the slot being overwritten while a zero-copy
    send's notification was still due.
17. `Backoff` is a state of connections the caller opened; their slots
    are not in the accept range.

## The listener state table

| State | `listen()` | `close_listener()`, `drain()` | accept ok | accept failed | Closed | deadline |
|---|---|---|---|---|---|---|
| `Free`, no failure owed | takes the slot. Bind ok: → `Listening`, an accept starts. In use, with a retry time: → `Binding`. Otherwise the error | `Stale`; a drain finds nothing to stop | — (a) | — (a) | — | — |
| `Free`, failure owed | not picked until `ListenFailed` has been pulled; pulling it retires the handle | the failure is forgotten | as above | as above | — | — |
| `Binding` | — | handle retired, → `Free`, nothing owed | — (b) | — (b) | — | bind again. Ok: → `Listening`. Still in use, time left: again in 50 ms. Otherwise: → `Free`, `ListenFailed` owed |
| `Listening` | — | handle retired, → `Closing`, the close is submitted | the connection takes its slot, or is parked if the slot is still closing; the wait after failures is reset; the next accept starts | cancelled: nothing. Otherwise wait (5 ms, doubling to 1 s; the length is kept until an accept works), then accept again | — | the wait is over: accept again; `poll` retries if the driver has no room |
| `Closing` | — | `Stale`; a drain finds nothing to stop | the connection is closed at once, unannounced | the reserved slot goes back; → `Free` if the close has completed | → `Free` once no accept is in flight (c) | —; every `poll` retries a close the driver had no room for |

a. A listener is `Free` only when its accept has made its last report.
   (The merged code frees it earlier: finding C6.)
b. No accept is started before the bind.
c. An accept's last report may come after the close's.

## The TLS phase table of the kernel transport
Per connection slot, in `kernel/secure.rs`. A phase other than `Plain`
routes the slot's send and receive completions through its stream.

| Phase | `secure` | a send or receive completion | `shutdown_write` | `close` submitted | Closed |
|---|---|---|---|---|---|
| `Plain` | the stream is reset, the handshake starts: → `Handshake` | reported as it is | a shutdown | stays (a slot that is not protected, or no longer) | stays |
| `Handshake` | — | fed to the handshake. Pending: nothing reported. Done: → `Secure`, `Secured` ok. Failed: → `Plain`, `Secured` failed | — (not up yet) | → `Aborted` | — (the close made it `Aborted`) |
| `Aborted` | — | → `Plain`, `Secured` reported as cancelled | — | stays | stays |
| `Secure` | — | send: fed to the stream; `Sent` when all of it has gone. receive: `Received` with plaintext; an end without a close alert is a reset | the close alert is sent: → `Alerting` | stays | → `Plain`, keys forgotten |
| `Alerting` | — | send: when the alert has gone, → `Secure` and the shutdown is submitted (`ShutDown` at once if the driver has no room). receive: as `Secure` | — (once) | → `AlertAborted` | — (the close made it `AlertAborted`) |
| `AlertAborted` | — | send: → `Plain`, `ShutDown` reported as cancelled. receive: reported as it is | — | stays | stays |

## Where the merged code fell short of the tables
Found by checking against the code, and closed in group 0. C1, C2, C4 and
C5 have scenario tests that failed before their fix. The test for C6 and
C8 passed on the old code too: the simulator does not deliver a closed
listener's last accept report late enough to show the fault, so that test
guards the rule without having reproduced the fault. The seeded
completion order of task 1.1 is what will.

- **C1 Half-close by the peer ends everything.** Decided: supported, as
  above (`Finishing`, `HalfClosed`, `Event::Finished`, the option).
- **C2 Frames are lost if the end event is pulled before them.** Decided:
  the end is held back until the input is pulled, as above.
- **C3 A held-back end had no bound without an idle timeout.** Settled by
  C2: holding the end is now the rule, not a special case, and note 15
  says what bounds it.
- **C4 A refused connection can be sent any number of frames.** To fix:
  one.
- **C5 `shutdown_write` answers `Stale` for a valid handle that is not
  up.** To fix: `NetError::NotUp`.
- **C6 A listener slot can be taken again before its accept has made its
  last report.** That report then clears the new listener's accept flag
  (a second accept is started while one runs), or hands the new listener
  a connection its predecessor accepted. To fix as for connections: a
  listener slot is `Free` only when its close has completed and no accept
  is in flight (the listener table says so).
- **C8 A connection outlives its listener's slot.** Found while fixing
  C6: a connection's events and its count against the listener's limit
  went by the listener's slot, so a connection announced or freed after
  its listener was closed named, or was counted against, whatever
  listener had the slot by then. Fixed with C6: a connection keeps the
  handle of the listener that accepted it.
- **C9 Connections could deadlock on frame memory.** Found by the model
  test: a frame that continued past a connection's backlog was put
  together in a second buffer, so as many connections as there are
  buffers each held one and waited for another, with the caller holding
  nothing. Fixed in group 1: the frame is put together in the backlog's
  own buffer, and a header that was collected in pieces goes back in
  front of what follows it there (the review of group 1 found that
  second case, which the first fix missed). A protected connection in
  ring mode does need two (the frame being put together, and the records
  that hold its rest): the last free buffer is kept for a connection
  that holds one already, so one of them can always finish. It is kept
  back only while another protected ring connection holds frame memory
  for undelivered input; the review of group 2 found that the first
  form of the rule withheld it from the only connection that wanted it,
  when the caller kept a frame of two.
- **C10 Connections waited on each other across the ring and frame
  memory.** Found while closing the open items of group 1, with more
  connections than frame buffers: those putting a frame together waited
  for its rest from the buffer ring, and the ring was held to the last
  buffer by the unpulled input of connections waiting for a frame
  buffer. Four connections on two buffers stopped for ever. Fixed: a
  connection whose armed receive has ended does not need the ring to
  finish. A plain one receives the rest of its frame straight into the
  frame's memory; a protected one receives into its stream's own buffer
  (`Stream::recv_carried`, `Transport::recv_kept`). The steady state is
  unchanged: an armed receive stays armed.
- **C11 A stream was asked for more while a record waited in it.** Found
  by the model over TLS, in the fix for C10: opening the record moved
  the bytes the receive was being written behind, the next record did
  not authenticate, and a connection that was up ended as a TLS failure
  or never heard of its peer's end (6 of 4000 runs). And the mark that
  the ring had been empty outlived its connection. Fixed: what a stream
  holds is opened first, the stream asserts it, and the mark goes with
  the rest of the receive state.
- **C12 A cancel for every pull that found no frame memory.** Found by
  the fuzz target `tcp_model_steps` in group 4: a pull that finds no
  frame memory stops the connection reading, which cancels its armed
  receive; a caller that pulled again before the next turn sent another
  cancel of the same receive, and nineteen of them left the backend no
  room for anything else. The model test's generator never pulls that
  often between two turns, so its hundred thousand seeds passed: what a
  seeded generator will not do, a fuzzer does. Fixed: one cancel for an
  armed receive. The input is kept with the model test, which replays
  every run a fuzzer found.
- **C13 Sends kept past a failure held the memory the last input
  needed.** Found by the same fuzz target, in the run that was to close
  group 4. A connection whose peer has reset keeps what it had queued to
  send until it is closed, and it is closed only when its caller has
  pulled its last input and its end. Frames lent for sending were among
  what two such connections had queued, all three frame buffers of the
  set in them; their last input, frames longer than a ring buffer,
  waited for frame memory, and they waited for ever, telling `Readable`
  at every turn. Fixed: what a connection has queued is let go as soon
  as it can never go out (it has failed, or its end is owed), a send in
  flight being waited for. Kept as a second run with the model test.
- **C7 The connect timeout marks the socket open whether or not there is
  one**, and relies on the backend reporting the close of an empty socket
  slot like any other. Kept, as assumption A2 of the conformance suite.

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
  comes at most once. `Finished` comes at most once per time the
  connection is up, only with half-close on, and before `Closed` or
  `Down`.
- **I6 Delivery.** Bytes are delivered exactly once, in order. No end
  event (`Finished`, `Closed`, `Down`) is owed while anything received is
  left to pull, in whatever order the caller pulls events and frames and
  whatever the pools hold at that moment. Only `close()` and the idle
  timeout drop input.
- **I7 The end.** `Orderly` only if the stream ended between frames (with
  TLS: after a close alert) and no send failed. Otherwise `Reset`, `Tls`,
  `Protocol` or `Timeout`. `Finished` only for an end that is in order.
- **I7a Half-close.** After `Finished` the connection can be sent on
  until the caller shuts it down or closes it; with the option off the
  peer's end is the connection's end.
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
| Frame memory (extents) | direct receive, assembling a frame, a spill, TLS opening, `hold`, a frame lent for sending (until it has gone, or its connection can send no more: C13) | the read pauses, the input stays where it is, a connection whose stream has ended stays `Finishing`; `hold` returns `NoMemory` | `release`, a send or write that drops its hold; `poll` retries |
| Frame memory, among connections | each connection's undelivered input | a connection holds at most one buffer for input it has not delivered (its backlog, or the frame it is putting together, or a whole frame waiting), so connections cannot each hold one and wait for a second; found by the model test, where three connections and three buffers stalled for ever. A protected connection in ring mode can hold two: while another such connection holds frame memory, the last free buffer goes only to one that holds one already; ring mode with TLS needs two buffers at least | — |
| Hold count of one extent | a frame lent from a backlog, `send_lent`, a file write | the frame is not lent and stays; `Stale` or `Full` to the caller | a hold released |
| Send segments | `send_frame`, `send_bytes` | `Full`; the connection joins the fixed waiter list | a segment returned: one waiter is told `Writable`, and a waiter that leaves passes it on |
| Send queue of a connection | every send | `Full` | its own send completing: `Writable` |
| Buffer ring | the kernel, for armed receives | the receive ends with "no buffers". A connection putting a frame together then receives its rest straight into the frame, and a protected one into its stream's buffer, so that neither waits for a ring that connections waiting for frame memory hold (C10) | `poll` arms again after giving buffers back; unpulled input is moved out at each `poll` |
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

As built, after group 1 and its open items: connect, listen, close,
close a listener, shutdown, send, pull events, pull frames, release, lend
to another connection and turn; one connection in eight reconnects; one
run in three is over TLS where that is compiled in, with frames longer
than a record and a fixed Ed25519 key, so that every handshake message
has the same length in every run and a failing seed replays. Every call
into a set runs under `alloc_guard`. An end as a protocol or TLS failure
is a fault, except a TLS failure of a connection that was never up. The
sum over the seeds is held against three tables: the connection state
table, the listener state table and the TLS phase table of the kernel
transport. Not in it: restart of a peer, a bind that is retried (no run
is ever `Binding`), and the stall of C9's TLS case, which needs frames of
several records on every connection at once and has a scenario of its
own.

Group 2 runs it under fault campaigns, each a setting of the simulator's
faults on top of the usual ones (reports out of order, receives and
sends cut short): `resets` often enough to fall inside frames, `slow`
with most receives and sends cut short, `delays` holding ready
operations back, `refusals` of connects, and `flips` of bits in transit
over TLS, where a TLS failure of a connection that was up is then an
allowed end and a frame with other bytes than were sent never is. The
adversary's own closes, the small pools and the end of the run put each
fault under a close, under exhaustion and at the end of a stream. Half
the runs end with a drain of the server, which must say so once. A run
returns a hash of every completion the backend delivered: the same
seed gives the same hash, another seed another. Not under `flips`: a
handshake carries random bytes, and what a flipped bit does there can
depend on the value it meets, so a seed of that campaign can take
another course the next time and may need several replays to fail
again. `cargo test` runs 150
seeds of the usual campaign and 40 of each other; CI a thousand more
(`-- --ignored a_thousand`); the nightly workflow a hundred thousand on
every core (`-- --ignored a_hundred_thousand`, four and a half minutes
on sixteen cores). `tcp_model --campaign NAME` replays one.

What the campaigns found was in the model, not in `Net`: a connection
that was refused for want of room, sends again without waiting and is
not refused has seen the room for itself, and is owed no `Writable` for
the refusal. `docs/tcp.md` says so now.

## The transport conformance suite
What `Net` relies on from any transport, as a suite a transport runs on
itself with no `Net` above it (`longshore_tcp::check::conformance`), so
that change 3's transport is held to it from its first commit. A
transport brings a rig: itself, its driver, and a way to turn the driver.

- Every operation that was started is reported once, as ended.
- A close is reported once. The operations it ends are reported too,
  before or after it, and then nothing more for the slot. (A transport
  may put a new connection into the slot before the last of those reports
  has come: `Net` parks it. What `Net` needs is only that each operation
  reports once.)
- A close of a slot that holds no socket is reported like any other
  (assumption A2).
- A connect whose slot is closed before it ends leaves no socket behind:
  the slot can connect again (assumption A1).
- A receive that ended at once is not also reported.
- After the end of the peer's stream is reported for a slot, sends on it
  still work and are reported, until the write side is shut down
  (half-close).

- An operation refused as busy has changed nothing: nothing is reported
  for it, nothing was sent, and it works once there is room. (A rig can
  take all the room its driver has.)
- On a rig whose transport has TLS for both ends: a handshake is
  reported once, as `Secured`, also when its slot is closed under it.
- The receives `Net` falls back to when the ring has nothing for a
  connection: when an armed receive has ended, a single receive works
  and is reported once; on a protected slot a receive into the
  transport's own memory is reported once, what it brought opens with no
  bytes, and one that a close ends is reported once. A transport without
  such memory refuses as unsupported, and `Net` waits for the ring.

## The packet device
`longshore_common::sim::PacketLink`: a link between two ends that carries
packets, for a transport that has a TCP of its own (change 3) to be
tested on, where `SimIo` stands in for the kernel's sockets. It stands
where the NIC and the wire do: an end hands it whole packets and takes
whole packets, and between the two a seeded generator loses, duplicates,
delays and reorders them.

It follows the rules of the rest: its packet memory and its queues are
built once, for a number of packets and a longest packet, and nothing
allocates afterwards; it does no I/O and has no clock, the caller gives
the time with every call; the same seed and the same calls give the same
packets, in the same order, at the same times.

### Where a packet is
A slot of the device's packet memory is in exactly one place.

| Place | It got there by | It leaves when | To |
|---|---|---|---|
| Free | being built; a packet delivered, taken or dropped | `send` | Queued |
| Queued (the sender's transmit queue) | `send` | the next `turn` | In flight; Dropped (lost) |
| In flight | a `turn` that sent it, with the time it is due; a duplicate is a second slot with the same bytes and a time of its own | a `turn` at or after its due time | Delivered; Dropped (the receive queue is full) |
| Delivered (the receiver's receive queue) | a `turn` | `recv` | Free |
| Dropped | loss, or a full receive queue | at once: it is counted, and its slot is Free | Free |

### What each call does

| Call | Free slots, and room in the queue | none |
|---|---|---|
| `send(from, bytes)` | the bytes are copied to a slot: → Queued | `Full`, nothing changes; `TooLong` for a packet longer than the device carries |
| `turn(now)` | every Queued packet of both ends, in the order sent: lost (→ Dropped), or → In flight due at `now` + the latency + its delay, and with it a duplicate if one is drawn and a slot is free. Then every In flight packet that is due, in the order of due time and then of sending: → Delivered, or → Dropped when the receive queue is full | a duplicate that finds no slot is not made (counted as none) |
| `recv(at, into)` | the oldest Delivered packet of that end is copied out: → Free. One longer than `into` stays, and `next_len` tells its length | `None` |
| `next_due(now)` | the earliest time a `turn` would do something: `now` if anything is Queued, else the earliest due time In flight | `None` |

Reordering is a delay: a packet drawn to be reordered is due later by a
fixed time, a millisecond unless set otherwise, and packets put in
flight within that time arrive before it. So it is out of order only
where its sender sends more often than that; the link counts the holds,
not the overtakings. Delay is a time drawn below a bound. With neither, and no
loss, packets arrive in the order sent, each once, a latency after the
`turn` that sent them.

### Invariants

- **P1 One place.** Every slot is Free, Queued, In flight or Delivered,
  and in one queue or list only. Free + Queued + In flight + Delivered is
  the number of slots, after every call.
- **P2 Conservation.** Packets accepted by `send`, plus duplicates made,
  equal packets taken by `recv`, plus packets dropped, plus those Queued,
  In flight and Delivered now.
- **P3 Nothing invented, nothing changed.** A packet taken by `recv` has
  the bytes and the length of a packet sent from the other end. The
  device flips no bits: corruption is not among its faults.
- **P4 Without faults: once and in order.** With every rate zero and room
  in the queues, each packet sent is delivered once, in the order sent.
- **P5 Not before its time.** No packet is Delivered earlier than the
  latency after the `turn` that sent it, nor later than the latency plus
  the longest delay plus the reordering time; and none is delivered
  before it was sent.
- **P6 Order is kept unless asked.** With the reordering and delay rates
  zero, packets of one direction are delivered in the order sent
  whatever is lost or duplicated, and a duplicate comes next to its
  original.
- **P7 The same seed, the same trace.** Two devices with the same seed,
  rates and sizes, given the same calls, hand out the same packets at the
  same calls and have the same trace hash; another seed gives another.
- **P8 The two directions do not mix.** What one end sends only the other
  receives. One draw sequence serves both, so what one direction does
  changes the faults of the other: that is what a seed fixes.
- **P9 Nothing allocates after it is built.**

### Exhaustion

| Pool | Who takes | When it is empty | What makes room |
|---|---|---|---|
| Slots (shared by both directions) | `send`; a duplicate | `send` answers `Full`; the duplicate is not made | `recv`; a loss; a drop |
| An end's transmit queue | `send` | `Full` | the next `turn` |
| An end's receive queue | a `turn` delivering | the packet is dropped and counted, as a NIC's receive ring that is not read drops | `recv` |

### How it is checked
Its own test binary, `sim_packet`, under `alloc_guard`: a scenario for P4,
for each fault alone with its rate held against the count over many
packets, for each row of the exhaustion table, and for P7; and a run of
seeded random calls (send from either end, turn, receive, time passing)
that checks P1, P2 and P3 after every call. The seed also chooses that
both ends receive at once after every turn, where P5's lower bound is
held at the turn that delivered, and that reordering and delay are off,
where P6 is held. P5's upper bound is held where turns come at a steady
pace.

## Decisions
- **The tables describe `Net` as it is to be.** Every cell that exists in
  the merged code was checked against it; `Finishing`, `HalfClosed` and
  the cells C1 to C7 name differ from it and are built in group 0. Where
  the model test finds the code disagreeing with a cell, the cell wins
  unless this document is changed first.
- **Half-close is an option, off by default**: a caller that never sends
  after the peer's end then has nothing to do at the end, as today.
- **The end is held by state, not by withdrawing an event**: `Finishing`
  replaces the flag that took an owed end back when frame memory ran out.
- **Flags to states, where the flags exclude each other.** Whether a
  slot's socket is open and whether its close has completed were two
  flags, of which one combination meant nothing and one was reached by a
  path that marked a socket closed while its close was still in flight.
  They are one state now (none, open, closing, closed). The three flags
  of why a connection does not read (paused for frame memory, records the
  stream still holds, a stream cut off without a close alert) stay flags:
  they do occur together, so an enum would have to name every
  combination. Whether a closing connection redials stays a flag too; it
  means something in one state only, and making it part of that state
  would touch every place that asks for the state.
- **Benchmarks** as in the proposal; unchanged by this revision.

## Risks / Trade-offs
- The model test can be wrong where the code is right → the reference is
  kept to a byte queue and the two tables, nothing cleverer.
- Seeded completion order in the simulator changes existing trace hashes →
  the knob is off unless a test asks for it.

## Open Questions
- Whether the model's step generator should be shared with change 3's
  packet-level tests, or kept apart.
