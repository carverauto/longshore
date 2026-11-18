## Context
`SimIo` implements `Io` in three files. `backend.rs` has `submit` (which
fixes a cancel's target and a timer's deadline) and `tick`. `engine.rs` has
`step`, one pass over the pending operations in an order drawn from the
seed, and `attempt`, which tries one operation against the simulated
sockets and files. `attempt` returns `Some(result)` or `None` for "not
ready". That is how a readiness backend works; the simulator's kernel is
simply imaginary.

The ring backend keeps the same contract in its own way, in
`io/uring/backend.rs` and `io/uring/completions.rs`.

## Goals / Non-Goals
- Goals: one implementation of the contract shared by the simulator and the
  portable backend; a backend that runs on macOS and on Linux without
  `io_uring`; no change to any existing seed's trace; the same tests on
  every backend
- Non-Goals: performance on the portable backend, which serves development,
  tests and hosts that refuse `io_uring`; zero-copy send, registered
  buffers and registered files outside the ring; file operations beyond
  `ReadFile`; a thread of any kind beside the one that calls `tick`

## Decisions
- The engine is generic over a world, `Engine<W: World>`, and implements
  `Io`. It owns the buffer pool, the pending table, the waker flag and the
  trace. `SimIo` becomes `Engine<Virtual>` under its present name, so tests
  and the crates above do not change.
  Alternative considered: a separate kqueue backend beside the ring and the
  simulator. Rejected because the contract would then live in three places.
- A world has four duties: attempt one operation, which may read and write
  the pool; say what time it is; wait until an operation may be ready or a
  deadline passes; and release what a cancelled operation held. The engine
  never touches a socket and a world never touches the pending table.
  Two places in today's code cross that line and are turned into calls: a
  cancelled connect gives back its socket slot, and a close ends a connect
  still waiting on that socket.
- The engine asks the world which pending operations to attempt. The
  virtual world answers "all of them, starting at a seeded offset", which
  is today's `step`, so the draws from the generator and the trace are
  unchanged. The POSIX world answers with the operations whose descriptor
  the kernel reported, and each new operation once.
- The POSIX world registers a descriptor for one report at a time
  (`EV_ONESHOT`, `EPOLLONESHOT`) and registers it again when an attempt
  returns "not ready". A report that turns out to be stale therefore costs
  one system call and loses nothing. This is the fault of
  compio-rs/compio#1119, where a stale event dropped the events after it.
- kqueue and epoll differ only in how they wait and wake: `EVFILT_USER`
  against an eventfd. That part is a small trait with two implementations.
  The attempts are POSIX calls and are shared.
- Results are negated Linux error numbers on every backend, as they are
  today (`io.rs:119`). The POSIX world translates at the one place it reads
  `errno`.
- `Send` with `zero_copy` set is sent by copy outside the ring and
  completes once, as the simulator does today.
- Timers are the engine's. The world is given the earliest deadline as the
  longest it may wait. No kernel timer is used outside the ring.
- System calls are declared by hand, as in `io/sys.rs`. `longshore_common`
  keeps no dependencies.
- On Linux the client uses the ring. The portable backend is used on Linux
  only when the builder asks for it. A ring that cannot be set up is an
  error that says why, as today, not a silent change of backend.
  Alternative considered: fall back when the kernel refuses `io_uring`.
  Rejected because the throughput figures would silently stop applying.

## Risks / Trade-offs
- An operation still has three bodies: the ring's, the virtual attempt and
  the POSIX attempt. Only the contract around them is shared. → The shared
  tests run every operation on all three.
- The split could change the order of draws from the generator and with it
  every trace. → A test records the trace of a set of seeds before the
  split and compares after it. The split lands alone, before the POSIX
  world.
- "Passes on kqueue" does not mean "correct on `io_uring`". → The
  real-kernel checks stay required. `scripts/test-linux.sh` no longer
  passes when no Linux host is configured.
- macOS error numbers and socket behaviour differ from Linux in places not
  yet met. → The shared tests are the place they surface.

## Migration Plan
1. Record traces. Split engine and world with the virtual world only.
   Compare traces.
2. Add the POSIX world with kqueue, then epoll.
3. Move the client's thread start off the Linux-only path.
Each step is its own commit and can be reverted alone.

## Open Questions
None. Moving the HTTP `Stream` (plain or TLS over `Io`) into a crate of its
own, so a server can use it, is decided and belongs to the later
networking change.
