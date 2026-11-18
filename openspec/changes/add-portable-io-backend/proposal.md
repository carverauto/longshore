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
  and the trace. A world attempts one operation and waits for something to
  become ready
- The virtual world is today's simulated sockets, files, faults and virtual
  time. `SimIo` keeps its name and its API, and every existing seed replays
  to the same trace
- A POSIX world attempts each operation with a non-blocking system call and
  waits with kqueue on macOS and epoll on Linux. It is the portable backend
- The contract every backend keeps is written down, and one set of tests
  runs against the ring, the portable backend and the simulator
- The S3 client runs on the portable backend on macOS, in place of the
  unsupported error planned as task 4.1 of `add-high-level-client`
- **BREAKING**: `Threads::start` no longer fails on a host other than Linux

## Impact
- Affected specs: `io-backends` (new)
- Affected code: `longshore_common` (`io`, `sim`), `longshore_s3`
  (`client/threads`), `scripts/test.sh`
- Supersedes: task 4.1 of `add-high-level-client`
- First principles: principle 1 changes from "`io_uring` completions" to
  "one completion queue"; `openspec/project.md` is updated with it. The
  performance figures in `bench/results/` are for `io_uring` and stay so.
  The portable backend allocates only when it is built, and its tests run
  under the allocator that aborts otherwise. No dependency is added
- Not in this change: file operations beyond `ReadFile`, the simulated disk
  moved from UltraGraphDB, multishot operations and buffer rings. Each is a
  later change on this engine
