## Context
`SimIo` implements `Io` in three files. `backend.rs` has `submit` (which
fixes a cancel's target and a timer's deadline) and `tick`. `engine.rs` has
`step`, one pass over the pending operations in an order drawn from the
seed, and `attempt`, which tries one operation against the simulated
sockets and files. `attempt` returns `Some(result)` or `None` for "not
ready". That is how a readiness backend works; the simulator's kernel is
simply imaginary.

The two halves are not cleanly apart today. One generator serves the walk's
starting point, a delay drawn for every operation visited, and every fault
inside `attempt`. The clock moves on every step. A connect that claimed a
socket is marked in the pending table from socket state. The whole of `sim`
compiles only for tests or under the `sim` feature.

The ring backend keeps the same contract in its own way, in
`io/uring/backend.rs` and `io/uring/completions.rs`.

## Goals / Non-Goals
- Goals: one implementation of the contract shared by the simulator and the
  portable backend; a backend that runs on macOS and on Linux without
  `io_uring`; no trace changed by the split; the same tests on every
  backend
- Non-Goals: performance on the portable backend, which serves development,
  tests and hosts that are asked to run without the ring; zero-copy send,
  registered buffers and registered files outside the ring; file operations
  beyond `ReadFile`; a thread of any kind beside the one that calls `tick`

## Decisions

### Engine and world
- The engine is generic over a world, `Engine<W: World>`, and implements
  `Io`. It lives in `io::engine`, always compiled. It owns the buffer pool,
  the pending table, operation numbers, the waker flag and the trace.
  `SimIo` becomes a name for `Engine<Virtual>` with its test-setup methods
  as inherent methods, so no test changes.
  Alternative considered: a separate kqueue backend beside the ring and the
  simulator. Rejected because the contract would then live in three places.
- The engine resolves timers, cancels and wakes itself. Everything else is
  the world's.
- The world owns the seeded generator and the faults. To keep every draw in
  today's order, the engine calls the world at the points where the
  simulator draws now, and nowhere else:
  1. `advance`, once per step, also when nothing is pending. The virtual
     world adds its time step.
  2. `first(n)`, the position the walk starts at.
  3. `hold(op)`, for every operation visited that is not cancelled,
     including the engine's own timers, cancels and wakes. The virtual
     world draws its delay here.
  4. `attempt(op, pool)`, for operations the engine does not resolve.
  5. `claimed(op)`, after an operation was not completed: whether it holds
     a resource that a cancel must give back. A waiting connect does.
  6. `release(op)`, when a cancelled operation that had claimed something
     completes.
  7. `closed(sock)`, after a close completes. The engine then marks the
     connect waiting on that socket as cancelled, as `attempt` does today.
  8. `wait(deadline, only_timers)`, when a tick that may wait has nothing
     to deliver. The virtual world jumps its clock when only timers remain
     and reports a deadlock after its bound of idle steps. The POSIX world
     sleeps in the kernel.
- The world also answers which operations to visit. The virtual world says
  all of them. The POSIX world says those whose descriptor may be ready.
  The engine always visits timers, cancels, wakes, cancelled operations
  and each new operation once, whatever the world says, since they have no
  descriptor to report them.

### Contract
- A close gives every other operation in flight on its socket its final
  completion without waiting for the peer: a negative result, or the real
  result if the operation had finished. No order is promised between those
  completions and the close's own.
  - Simulator: a receive or send on a closed socket already completes with
    an error at its next visit. An accept in flight on a closed listener
    now completes too.
  - Ring: the backend records the socket of each operation in flight and
    cancels those on a socket when its close is submitted.
  - POSIX world: the engine visits them at the next step, and the world
    finds the slot closed.
- The result of closing a slot that holds no socket is not part of the
  contract. The close still completes once.
- A cancel completes with zero when it found its target and with a negative
  result when it did not. Which negative result is not part of the
  contract; the ring and the simulator differ.
- Results are negated Linux error numbers on every backend. The POSIX world
  translates by name where it reads `errno`, for the whole set of socket
  and file errors, and gives the I/O error for one it does not know. The
  numbers reach users through the HTTP client's error type, so a partial
  table would mislabel errors.

### POSIX world
- Socket attempts are `connect`, `accept`, `send`, `recv` and `close` on
  non-blocking sockets. Sends suppress `SIGPIPE`. Accepted sockets are set
  non-blocking explicitly, which Linux does not inherit.
- `ReadFile` reads a regular file and blocks the calling thread for that
  read. Regular files have no readiness.
- Readiness is remembered. The world keeps, for each socket and direction,
  whether it may be ready. A report from the kernel sets it and only an
  attempt that returns "not ready" clears it. A report that arrives when
  the completion buffer is full is therefore not lost.
- kqueue registers each direction of a socket for one report, and
  registers it again after an attempt that was not ready. The two
  directions are independent there.
- epoll does not use one-shot mode, which disarms the whole descriptor and
  would lose the other direction's operation. A descriptor is registered
  for the directions that have an operation waiting and not yet reported,
  and the registration is changed when that set changes.
- Every tick polls the kernel, with no wait when completions are already
  to hand. A caller that keeps submitting still gets its socket
  completions.
- A wait that a signal interrupts is retried, as the ring does.
- The waker is the engine's flag together with a descriptor of the
  world's: a user event on kqueue, an eventfd on epoll. The waker owns a
  duplicate of that descriptor, so it outlives the backend safely.
- `Send` with `zero_copy` set is sent by copy and completes once.
- Timers are the engine's. The world is given the earliest deadline as the
  longest it may wait. No kernel timer is used.
- A connect does not measure segment sizes on this backend and completes
  with zero, which the contract allows.
- System calls and constants are declared by hand, one module for macOS
  and one for Linux, since socket address layouts and constant values
  differ. `longshore_common` keeps no dependencies.
- A test-only hook in the waiter adds a report for a descriptor that is
  not ready, under the `fault-injection` feature. The stale-report tests
  use it.

### Client
- The thread start becomes generic over a small trait: build a backend
  from `Limits`, and quiesce. The engine's quiesce succeeds at once,
  because the kernel touches a buffer only inside a system call.
- On Linux the client uses the ring. The portable backend is used on Linux
  only when the builder asks for it. A ring that cannot be set up is an
  error that says why, not a silent change of backend.
  Alternative considered: fall back when the kernel refuses `io_uring`.
  Rejected because the throughput figures would silently stop applying.
- On macOS the client uses the portable backend. A request to pin threads
  to cores fails the build with a setting error there, since macOS cannot
  honour it.
- `BuildError::Unsupported` stays, for hosts that are neither.
- Fault injection into the client stays a feature of the ring backend.

### Limits
- `Limits` gains a constant base value, and every place that builds a
  `Limits` names its fields over that base. There are about thirty, most
  in tests and the benchmark programs. Later changes add fields without
  touching them again.

## Risks / Trade-offs
- An operation still has three bodies: the ring's, the virtual attempt and
  the POSIX attempt. Only the contract around them is shared. → The shared
  tests run every operation on all three.
- The split could change the order of draws and with it every trace. → The
  split lands alone, with the world called at exactly today's points, and
  is compared against recorded traces before anything else changes.
- The close rule changes what the ring delivers. → It lands as its own
  step after the split, with the client's real-kernel checks run before
  and after.
- "Passes on kqueue" does not mean "correct on `io_uring`". → The
  real-kernel checks stay required. `scripts/test-linux.sh` no longer
  passes when no Linux host is configured.

## Migration Plan
1. `Limits` base value. No behaviour changes.
2. Record traces. Split engine and world with the virtual world only.
   Compare traces: none may differ.
3. The close rule, in the simulator and the ring.
4. The POSIX world with kqueue, then epoll.
5. The client's thread start.
Each step is its own commit and can be reverted alone.

## Open Questions
None. Moving the HTTP `Stream` into a crate of its own belongs to
`add-server-networking`.
