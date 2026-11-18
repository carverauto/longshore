## 1. Engine and world
- [ ] 1.1 Record the trace hash of a fixed set of seeds across the existing simulation tests, as a test that fails if any changes
- [ ] 1.2 `World` trait: attempt, time, wait, release on cancel; `Engine<W>` implementing `Io` with the pool, pending table, waker flag and trace
- [ ] 1.3 Virtual world from today's `SimIo`; `SimIo` kept as a name for `Engine<Virtual>` with its test-setup methods
- [ ] 1.4 The two crossings (cancelled connect, close ending a waiting connect) as calls between engine and world
- [ ] 1.5 Traces of 1.1 unchanged; no test outside `longshore_common` edited

## 2. Contract
- [ ] 2.1 Shared tests generic over `Io`, grown from `sim/scenario.rs`: one completion, `Busy`, each cancel case, timers, wake, short sends and receives, close with operations in flight
- [ ] 2.2 Shared tests run against the simulator in `cargo test` and against the ring as a real-kernel check binary
- [ ] 2.3 `scripts/test-linux.sh` fails when no host is configured; the README says how to point it at a local Linux machine

## 3. POSIX world
- [ ] 3.1 System call declarations for macOS and Linux, with size checks, and the error number translation
- [ ] 3.2 Attempts for connect, accept, send, receive, receive-now, close, UDP connect and `ReadFile`; `listen`
- [ ] 3.3 Waiting on kqueue: one report per registration, register again on "not ready", wake by user event
- [ ] 3.4 Waiting on epoll, with an eventfd for wake
- [ ] 3.5 Stale-report tests, seeded: a report with nothing behind it among real ones, and alone
- [ ] 3.6 Shared tests of 2.1 against the portable backend on macOS and on Linux
- [ ] 3.7 Echo scenario under the allocator that aborts, on both

## 4. Client
- [ ] 4.1 Thread start generic over the backend; macOS uses the portable backend; a builder setting chooses it on Linux
- [ ] 4.2 Client tests against the local test server on macOS
- [ ] 4.3 Mark task 4.1 of `add-high-level-client` as superseded by this change

## 5. Project
- [ ] 5.1 `openspec/project.md`: principle 1, the constraint on other hosts, and the stale lines on the crate layout and `crc-fast`
- [ ] 5.2 README: what runs where, and that the figures are for `io_uring`
- [ ] 5.3 Run the real-kernel checks on Linux 6.1 and on a current kernel; record the result in `bench/results/`
- [ ] 5.4 Before and after figures for the S3 path on `io_uring`, to show the change costs nothing there
