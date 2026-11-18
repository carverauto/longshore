## 1. The crate
- [ ] 1.1 `longshore_mailbox`: the switch between standard and model primitives, moved from `client/sync.rs`
- [ ] 1.2 The bounded queue over those primitives; sender handle; refusal when full or closed
- [ ] 1.3 Receiver side: drain on `AwaitWake`, generic over `Io`
- [ ] 1.4 `Pool` with a bounded waiter queue, the cell per slot, the future with a blocking `wait`, generic over the answer
- [ ] 1.5 Closing: refuse senders, answer waiters with the closed error

## 2. Checks
- [ ] 2.1 Model tests for the queue: two senders and a receiver, full, closed
- [ ] 2.2 Model tests for the slots and closing, moved from `tests/client/model.rs` and made generic
- [ ] 2.3 `scripts/test-model.sh` runs the new crate's model tests
- [ ] 2.4 Simulation test: two backends on one thread exchanging messages, replayed by seed
- [ ] 2.5 Tests under the allocator that aborts

## 3. S3 client
- [ ] 3.1 `Lane`, `Pending` and the worker on the mailbox; `Request`, dispatch, lending and counters stay
- [ ] 3.2 Shutdown as closing
- [ ] 3.3 Client tests and the nine model tests pass unchanged
- [ ] 3.4 The threaded client's tests run under the allocator that aborts
- [ ] 3.5 Real-kernel checks, and S3 figures before and after on `io_uring`

## 4. Project
- [ ] 4.1 `longshore` re-exports the crate; README: passing messages between cores
- [ ] 4.2 The check binaries' own copies of `block_on` use the crate's
