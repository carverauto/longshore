## 1. The crate
- [x] 1.1 `longshore_mailbox`: the switch between standard and model primitives moved from `client/sync.rs` and made public, with a cell the model can track; the `longshore_model` cfg declared for the crate
- [x] 1.2 The bounded queue over those primitives, with one place kept for a stop; the sender handle: send, wake, stop; refusal when full or closed
- [x] 1.3 Close atomic with a send
- [x] 1.4 Receiver side: drain on `AwaitWake`, generic over `Io`; receive by parking, without an `Io`
- [x] 1.5 `Pool` with a waiter queue preallocated for a number set at build; the cell with a sent state and the caller's number
- [x] 1.6 The building blocks: take or wait for a slot on one pool or several, send, poll or wait for the answer, give up; the finished future built from them
- [x] 1.7 Ending: answers first, then the closed error for slots that were sent and not answered

## 2. Checks
- [x] 2.1 Model tests for the queue: two senders and a receiver; full; stop with messages before and after it; a send racing a close
- [x] 2.2 Model tests for the slots: two waiters on one slot, a caller that gives up, a future and a blocking caller, closing with and without an answer
- [x] 2.3 `scripts/test-model.sh` runs the new crate's model tests; the run time before and after recorded in the pull request
- [x] 2.4 Simulation test, left out of model builds: two backends on one thread exchanging messages, ticked without waiting, replayed from the same seeds and tick order
- [x] 2.5 Tests under the allocator that aborts, for the queue and for waiting within the preallocated number

## 3. S3 client
- [x] 3.1 `Request` derives `Copy`; `Lane` embeds the mailbox and keeps lending, counters and its closed-lane policy; `Pending` and the buffer wait on the building blocks
- [x] 3.2 Dropping the client asks for a stop; a failed thread closes, answers, and parks on the mailbox
- [x] 3.3 The builder's setting for preallocated waiters
- [x] 3.4 The client's tests and its ten model tests pass unedited
- [x] 3.5 The threaded client's tests run under the allocator that aborts, with each thread's waker made before the seal
- [x] 3.6 Real-kernel checks, and S3 figures before and after on `io_uring` *(31/31 real-kernel checks on `donbot` and `10.0.0.3` plus before/after `http_uring` loopback figures on `donbot` recorded in `bench/results/m5b-http-uring-donbot.jsonl` and `bench/results/m5b-core-mailbox.md`; no live S3 bucket credentials configured, so `http_uring` loopback + `client_uring`/`client_faults` verify the mailbox-backed S3 client on `io_uring`)*

## 4. Project
- [x] 4.1 `longshore` re-exports the crate; README: passing messages between cores
- [x] 4.2 `client_uring`'s own copy of `block_on` uses the crate's
