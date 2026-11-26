Each group ends with a review of its own diff (no-mistakes or
`/code-review`); its findings are fixed before the next group starts.
Group 1 may end the change: if no host at hand takes the registration,
its write-up is the result.

## 1. Spike on the Spark pair
- [x] 1.1 A standalone program (throwaway C in `bench/spikes/zcrx/`, not in `longshore-bench`: it was compiled on the host): register one receive queue for zero-copy receive, receive a stream from the other Spark, count bytes and CPU; the card settings it needed, written down
- [x] 1.2 The same stream with chunk lengths above the page size, if the driver grants them (it does not: 8 KiB to 2 MiB on huge pages are refused with "operation not supported")
- [x] 1.3 Result in `bench/results/` (milestone 31): taken or refused and why; CPU-seconds a GB against 0.13 to 0.16 of milestone 30; go or stop recorded in `design.md`
- [x] 1.4 Review of group 1

## 2. The contract
- [ ] 2.1 (first contract: not passed; rewritten as "The contract", C1 to C12 in `design.md`) Reviewed again against the kernel's source and the code; corrections made there first
- [ ] 2.2 `SimIo`: the receive area of C12: `Op::ArmedRecvZc` with a credit, pieces of seeded length, split pages, frames in and out of step, the copy path and its `-ENOMEM`, a full completion ring, the reference count a page, the marker at `give_back`, a small refill ring read lazily with its flush; `Op::FileWriteSpans` on the simulated disk; same seed, same trace
- [ ] 2.3 The model test and the conformance suite take the mode, with the checks of Z1 to Z12 after every step (failing until group 3)
- [ ] 2.4 Review of group 2

## 3. The mode
- [ ] 3.1 `longshore_common`: area and refill ring allocated at build from `Limits`, registration, `Op::ArmedRecvZc` with a credit, `Completion::off`, `give_back` of a piece through the backend's queue and the refill ring with the flush, 32-byte completions only with the mode; on the Spark, what a ring is told when its queue is reconfigured (K10), and the `Lost` row of C8 written from it
- [ ] 3.2 Refusals of C1 and K8: each build refusal named in `BuildError`, the card's split setting read over netlink before registering; the refusals at `connect` and `listen`
- [ ] 3.3 `longshore_tcp`: `Rx::ZeroCopy` as C4 and C5: the two armings a frame, the landing, the decision, kept and moved frames, the break, the ends of an armed receive; runs, tail blocks and the page budget
- [ ] 3.4 `Mem::Pages`: `Net::pages`, counted holds (`net.pages().hold`), `release` by generation, `hold` moving once, `Frame::span` and `Frame::extent` for a frame in pages, `send_lent` refusing one; `Usage`'s new counters
- [ ] 3.5 `Op::FileWriteSpans` on the ring and the portable backend; a frame in pages written with direct I/O and synced; on the Spark, whether the area can back a zero-copy send (recorded, for a later change)
- [ ] 3.6 Every row of C9 and C11 reached by a test; every invariant of C10 checked by the model test, each with a fault put in to show the check fails; allocation guard over the mode
- [ ] 3.7 `tcp_uring`: `--rx zc`, a log written from pages, and the real-kernel self-check run with it where the host can
- [ ] 3.8 Review of group 3

## 4. Zero-copy sends over a card
- [ ] 4.1 Where a zero-copy send of 64 KiB waits between its completion and its notification, traced on the Spark pair
- [ ] 4.2 The fix it points to; defaults for segment length and sends in flight
- [ ] 4.3 The wire matrix of milestone 30 again, five runs a cell: no frame size slower with zero-copy than without, by the medians
- [ ] 4.4 Review of group 4

## 5. Measurement and the decision on AF_XDP
- [ ] 5.1 Spark pair, both interfaces, MTU 9000 and 8244: GB/s a receiving core and cores at the full link, counting the queues' cores, `Ring`, `Direct` and `ZeroCopy`, frames of 324 bytes, 64 KiB and 1 MiB, checked and written to a log with several writes in flight, read in place, and relayed after `hold`; the share of frames lent in pages for a sender that starts each frame on a packet and one that does not; three runs a cell
- [ ] 5.2 Results in `bench/results/`; card settings and the support matrix in `docs/tcp.md`
- [ ] 5.3 `add-tcp-af-xdp-backend` updated with what is left for it to win, or closed
- [ ] 5.4 Review of group 5

## 6. The body at a block, in direct mode (first, by the owner's decision)
- [x] 6.1 `HeaderFraming::with_body_at_block`: in direct mode the frame is received with its header ending at a block boundary of its frame memory (for a 256-byte header, 3,840 bytes in); opening a connection refuses frame memory too small for the lead and the largest frame
- [x] 6.2 Ring mode refuses the option (a frame there is lent from memory it shares, at whatever offset it lies; only direct mode gives every frame memory of its own)
- [x] 6.3 Tests: the body's offset for any framing, block and cut of the stream (a property test, checked to fail without the lead), under TLS, and the two refusals
- [x] 6.3a The model test runs with the option on half its direct connections, a thousand seeds of every campaign (the conformance suite is of transports, below where the option lives: nothing of it applies)
- [x] 6.4 `tcp_uring path --header 256 --body-at block|frame`; measured on the Spark pair: about 5 % less CPU a block on the node, against the test program's 6 % (milestone 31)
- [x] 6.5 `docs/tcp.md`; review of group 6 (findings fixed: the refusal asked whether there was a lead, not whether the option was asked for; its test could not fail; the claim that nothing is moved does not hold under TLS)
