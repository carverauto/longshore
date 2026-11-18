## 1. The crate
- [ ] 1.1 `longshore_mailbox`: the switch between standard and model primitives moved from `client/sync.rs` and made public, with a cell the model can track; the `longshore_model` cfg declared for the crate
- [ ] 1.2 The bounded queue over those primitives, with one place kept for a stop; the sender handle: send, wake, stop; refusal when full or closed
- [ ] 1.3 Close atomic with a send
- [ ] 1.4 Receiver side: drain on `AwaitWake`, generic over `Io`; receive by parking, without an `Io`
- [ ] 1.5 `Pool` with a waiter queue preallocated for a number set at build; the cell with a sent state and the caller's number
- [ ] 1.6 The building blocks: take or wait for a slot on one pool or several, send, poll or wait for the answer, give up; the finished future built from them
- [ ] 1.7 Ending: answers first, then the closed error for slots that were sent and not answered

## 2. Checks
- [ ] 2.1 Model tests for the queue: two senders and a receiver; full; stop with messages before and after it; a send racing a close
- [ ] 2.2 Model tests for the slots: two waiters on one slot, a caller that gives up, a future and a blocking caller, closing with and without an answer
- [ ] 2.3 `scripts/test-model.sh` runs the new crate's model tests; the run time before and after recorded in the pull request
- [ ] 2.4 Simulation test, left out of model builds: two backends on one thread exchanging messages, ticked without waiting, replayed from the same seeds and tick order
- [ ] 2.5 Tests under the allocator that aborts, for the queue and for waiting within the preallocated number

## 3. S3 client
- [ ] 3.1 `Request` derives `Copy`; `Lane` embeds the mailbox and keeps lending, counters and its closed-lane policy; `Pending` and the buffer wait on the building blocks
- [ ] 3.2 Dropping the client asks for a stop; a failed thread closes, answers, and parks on the mailbox
- [ ] 3.3 The builder's setting for preallocated waiters
- [ ] 3.4 The client's tests and its ten model tests pass unedited
- [ ] 3.5 The threaded client's tests run under the allocator that aborts, with each thread's waker made before the seal
- [ ] 3.6 Real-kernel checks, and S3 figures before and after on `io_uring`

## 4. Project
- [ ] 4.1 `longshore` re-exports the crate; README: passing messages between cores
- [ ] 4.2 `client_uring`'s own copy of `block_on` uses the crate's
