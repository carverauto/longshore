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
deadlines. "—" is "cannot happen", with the reason in the notes; "ignore"
means the event is consumed and changes nothing but the in-flight flag it
belongs to. `free?` stands for "free the slot if the close has completed
and nothing is in flight"; a slot is freed in no other way.

| State | `close()` | `shutdown_write()` | send call | `next_frame()` | Connected ok / failed | Secured ok / failed | data received | end of stream | receive error | Sent ok / failed | Closed | deadline |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `Free` | stale handle | stale handle | stale handle | stale handle | — (1) | — (1) | — (1) | — (1) | — (1) | — (1) | — (1) | — (2) |
| `Connecting` | → `Closing`, no event | `NotUp` | `NotUp` | none | ok: TLS → start handshake, else → `Up` + `Up` event; failed: → `Dying` (`Reset`) | ok: → `Up` + `Up` event; failed: → `Dying` (`Tls` or `Reset`) | — (3) | — (3) | — (3) | — (3) | — (4) | connect timeout: → `Dying` (`Timeout`) |
| `Securing` | — (5) | — (5) | — (5) | — (5) | — (6) | ok: admit (→ `Up` + `Accepted`, or → `Refused` + `Refused` at the limit); failed: → `Closing`, no event | — (3) | — (3) | — (3) | — (3) | — (4) | handshake timeout: → `Closing`, no event |
| `Up` | → `Closing`, no event | mark; shut down when the queue is empty | queue, or `Full` | frames in order | — (7) | — (7) | mark `Readable` once | finish (8) | → `Dying` (`Reset` or `Tls`) | ok: next span, `Writable` if owed; failed: → `Dying` | — (4) | idle timeout: → `Dying` (`Timeout`) |
| `Refused` | → `Closing`, no event | `NotUp` | one frame | none | — (7) | — (7) | — (9) | — (9) | — (9) | ok: → `Closing` when the queue is empty; failed: → `Closing` | — (4) | — (2) |
| `Dying` | → `Closing`, no event | `NotUp` | `NotUp` | frames already received | ignore, `free?` | ignore, `free?` | ignore | ignore | ignore | ignore | — (4) | — (2) |
| `Closing` | handle already stale; cancels a redial (10) | stale handle | stale handle | stale handle | ok: the socket exists again, close it; `free?` | `free?` | return the buffer, `free?` | `free?` | `free?` | `free?` | `free?`, then → `Free` or → `Backoff` | — (2) |
| `Backoff` | → `Free` | `NotUp` | `NotUp` | none | — (1) | — (1) | — (1) | — (1) | — (1) | — (1) | — (1) | → `Connecting` |

`Dying` becomes `Closing` when the caller pulls the event that reports the
end (`Closed`, or `Down` for a reconnecting connection).

Notes:
1. Nothing is in flight for a `Free` or `Backoff` slot: a slot gets there
   only through `free?`. This is invariant I3, and the reason a transport's
   late completion can never reach the slot's next occupant.
2. The slot has no deadline armed in this state (I9).
3. No receive or send is started before the connection is `Up`. During a
   handshake the transport's own operations end in `Secured`, never in a
   receive or send report.
4. `Closed` is reported for a close `Net` asked for, and `Net` asks only
   on the way into `Closing`.
5. The caller has no handle to a connection in `Securing`.
6. An accepted connection is connected when it is reported.
7. A connect and a handshake are over before `Up` and `Refused`.
8. Finish: everything received before the end is cut into frames and
   offered first. Then `Orderly` if the input ended between frames (and,
   with TLS, a close alert was received), `Reset` if it ended inside one.
   When frame memory is exhausted the end waits for it (I6).
9. A refused connection does not read.
10. `close()` on a connection that is `Closing` after `Down` must stop the
    redial: the caller closed it.

## The listener state table

| State | `listen()` picks the slot | `close_listener()` | accept ok | accept failed | Closed | deadline |
|---|---|---|---|---|---|---|
| `Free`, no failure owed | bind: → `Listening`, or → `Binding` on "in use" with a retry time, or the error | stale handle | — (a) | — (a) | — (a) | — |
| `Free`, failure owed | not picked until `ListenFailed` has been pulled (b) | stale handle | — (a) | — (a) | — (a) | — |
| `Binding` | — | → `Free`, nothing owed | — (a) | — (a) | — (a) | bind again; still in use and time left: again later; out of time: → `Free`, `ListenFailed` owed |
| `Listening` | — | cancel the accept, → `Closing` | set the connection up; start the next accept | wait (5 ms, doubling to 1 s), then accept again; the wait survives a successful restart | — | the wait is over: accept again |
| `Closing` | — | stale handle | the connection is closed at once, unannounced | ignore | → `Free` once no accept is in flight | retry a close the driver had no room for |

a. No accept is in flight for a listener that is not `Listening` or
   `Closing`.
b. A handle is retired when its failure is pulled, so an old handle never
   names a new listener (I4).

## Invariants
Checked after every step of the model test, and by debug assertions where
that is cheap.

- **I1 Slot ownership.** A connection slot is owned by at most one
  connection; it is freed only when its close has completed and nothing is
  in flight for it.
- **I2 One in flight.** Per connection at most one send, one receive (or
  one armed receive), and one of connect, handshake, shutdown and close.
- **I3 Quiet when free.** Nothing is in flight, queued or held for a slot
  that is `Free` or `Backoff`.
- **I4 Handles.** A retired handle reaches nothing. A handle the caller
  still holds never comes to name another connection or listener.
- **I5 Events.** No event for a retired handle. `Closed` is the last event
  of a connection. `Up` and `Down` alternate. `Accepted` or `Refused`
  comes at most once.
- **I6 Delivery.** Bytes are delivered exactly once, in order. Everything
  received before the end of the stream is offered before the end is
  reported, whatever the pools hold at that moment.
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

## Decisions
- **The tables describe `Net` as built**, with the review's fixes. Where
  the model test finds the code disagreeing with a cell, the cell wins
  unless this document is changed first.
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
