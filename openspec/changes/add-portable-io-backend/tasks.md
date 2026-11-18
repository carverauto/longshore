## 1. Limits
- [ ] 1.1 `Limits` base value; every literal in the workspace, the tests and the benchmark programs names its fields over it

## 2. Engine and world
- [ ] 2.1 Trace hashes asserted in `longshore_common`'s simulation tests for a fixed set of seeds, with and without the delay fault, covering every operation kind
- [ ] 2.2 A one-off comparison of the DNS, HTTP and S3 simulation suites before and after the split: the same completions in the same order. Its result is recorded in the pull request, not committed as values
- [ ] 2.3 `io::engine`, always compiled: `World` trait with the eight calls of the design, `Engine<W>` implementing `Io`
- [ ] 2.4 Virtual world from today's `SimIo`, holding the generator and the faults; `SimIo` as a name for `Engine<Virtual>` with its test-setup methods; both behind the `sim` feature
- [ ] 2.5 `engine.rs` split by responsibility to stay under the file-size rule
- [ ] 2.6 Hashes of 2.1 unchanged and comparison of 2.2 clean; no test outside `longshore_common` edited

## 3. Contract
- [ ] 3.1 Close ends operations in flight: simulator (accept on a closed listener), ring (socket recorded per operation, cancelled at close)
- [ ] 3.2 Doc comments on `Op::Close` and `Op::Cancel` state the contract
- [ ] 3.3 Shared tests generic over `Io`, taking the address to use, grown from `sim/scenario.rs`: one completion, `Busy`, each cancel case, close with a receive and with an accept in flight, timers, wake. They tolerate short sends and receives
- [ ] 3.4 The shared tests as a real-kernel check binary, added to the build, copy and run lists of `scripts/test-linux.sh` and to the `uring` job of `ci.yml`
- [ ] 3.5 `scripts/test-linux.sh` fails when no host is configured; the README says how to point it at a local Linux machine
- [ ] 3.6 The client's real-kernel checks pass before and after 3.1

## 4. POSIX world
- [ ] 4.1 System call and constant declarations, one module for macOS and one for Linux, with size checks; the error translation by name with its fallback
- [ ] 4.2 Attempts for connect, accept, send, receive, receive-now, close, UDP connect and `ReadFile`; `listen`; no `SIGPIPE`; accepted sockets non-blocking; close of an empty slot
- [ ] 4.3 Remembered readiness per socket and direction; a poll on every tick; retry on an interrupted wait
- [ ] 4.4 kqueue: one report per direction, registered again when not ready; wake by user event; the waker owns its descriptor
- [ ] 4.5 epoll: registration that follows the waiting directions, no one-shot mode; wake by eventfd
- [ ] 4.6 The report-injection hook under `fault-injection`, and the seeded stale-report tests: among real reports, and alone
- [ ] 4.7 Tests for a full completion buffer and for a caller that keeps submitting
- [ ] 4.8 Shared tests of 3.3 against the portable backend on macOS and on Linux
- [ ] 4.9 Echo scenario under the allocator that aborts, on both
- [ ] 4.10 A macOS job in `ci.yml`: tests and clippy for `longshore_common`

## 5. Client
- [ ] 5.1 A backend trait (build from `Limits`, quiesce) and the thread start generic over it; fault injection stays with the ring
- [ ] 5.2 macOS uses the portable backend; the builder field that asks for it on Linux; pinning refused on macOS
- [ ] 5.3 A client test against the HTTP test server over loopback on the portable backend, run on macOS
- [ ] 5.4 Mark task 4.1 of `add-high-level-client` as superseded by this change

## 6. Project
- [ ] 6.1 `openspec/project.md`: principle 1, the constraint on other hosts, and the stale lines on the crate layout and `crc-fast`
- [ ] 6.2 README: what runs where, and that the figures are for `io_uring`
- [ ] 6.3 Run the real-kernel checks on Linux 6.1 and on a current kernel; record the result in `bench/results/`
- [ ] 6.4 Before and after figures for the S3 path on `io_uring`, to show the change costs nothing there
