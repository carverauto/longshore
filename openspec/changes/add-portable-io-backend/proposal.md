## Why
longshore runs on `io_uring` and nowhere else. On macOS, and in a container
whose seccomp profile refuses `io_uring`, the only backend is the simulator,
so nothing there touches a real socket. UltraGraphDB is moving its
networking and storage I/O from compio to longshore
(https://github.com/carverauto/longshore/issues/40) and is developed on
macOS, so it needs a backend that runs there.

A third backend written from scratch would give every operation a third
implementation of its contract (cancellation, timers, completion order,
back pressure), and the three would drift. The simulator already has the
shape of a readiness backend: `attempt` tries one pending operation and
returns `None` when it is not ready (`sim/simio/engine.rs:41`), and `step`
retries the pending ones. Only what `attempt` talks to is imaginary.

## What Changes
- The simulator is split into an engine and a world. The engine holds the
  pending operations, cancellation, timers, back pressure, completion order
  and the trace. A world attempts one operation, holds the seeded
  generator, tells the time and waits for something to become ready
- The engine moves out of the `sim` feature into code that is always
  compiled. The virtual world, its faults and the name `SimIo` stay behind
  the feature
- The virtual world is today's simulated sockets, files, faults and virtual
  time. `SimIo` keeps its name and its API. The split alone changes no
  trace
- A POSIX world attempts socket operations with non-blocking system calls
  and waits with kqueue on macOS and epoll on Linux. It is the portable
  backend
- The contract every backend keeps is written down, including what a close
  does to operations in flight on its socket, which the backends answer
  differently today
- One set of tests runs against the ring, the portable backend and the
  simulator. CI gains a macOS job for the portable backend
- The S3 client runs on the portable backend on macOS
- `Limits` gains a base value, and every place that builds one is moved to
  it, so later changes can add fields without breaking them
- **BREAKING**: building a client on macOS succeeds where it returned
  `BuildError::Unsupported`; `Builder` gains a public field that asks for
  the portable backend
- **BREAKING**: on the ring, a close now ends the operations in flight on
  its socket. Before, they stayed pending until the peer acted

## Impact
- Affected specs: `io-backends` (new)
- Affected code: `longshore_common` (`io`, `sim`), `longshore_s3`
  (`client/threads`), every crate's tests and `longshore-bench` (the
  `Limits` base value only), `scripts/test-linux.sh`,
  `.github/workflows/ci.yml`
- Supersedes: task 4.1 of `add-high-level-client`
- First principles: principle 1 changes from "`io_uring` completions" to
  "one completion queue"; `openspec/project.md` is updated with it. The
  performance figures in `bench/results/` are for `io_uring` and stay so.
  The portable backend allocates only when it is built, and its tests run
  under the allocator that aborts otherwise. No dependency is added
- Not in this change: file operations beyond `ReadFile`, the simulated
  disk, operations with several completions. Each is a later change on
  this engine
