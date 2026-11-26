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
- [ ] 2.1 (written and reviewed once: not passed, see "Review of the contract" in `design.md`) `design.md`: the ownership table of chunks, the states of a queue's registration, the invariants and the exhaustion rows, reviewed before code; first, from the kernel's source and a run on the Spark, what the kernel does when the area is empty and how many returns can be outstanding
- [ ] 2.2 `SimIo`: `Rx::ZeroCopy` receives delivered in chunks with seeded boundaries and seeded exhaustion; same seed, same trace
- [ ] 2.3 The model test and the conformance suite take the mode (failing until group 3)
- [ ] 2.4 Review of group 2

## 3. The mode
- [ ] 3.1 `longshore_common`: area and refill ring allocated at build from `Limits`, registration, `Op::ArmedRecvZc`, chunks returned without a system call, 32-byte completions only with the mode
- [ ] 3.2 Refusals: kernel, driver, privilege, header and data split, each named in `BuildError`; the card's split setting read over netlink before registering, since the registration itself only says `EINVAL`
- [ ] 3.3 `longshore_tcp`: `Rx::ZeroCopy`, frames in one chunk lent in place, spanning frames lent as pieces (`Net::pieces`), `hold` moving once, chunk accounting as one structure
- [ ] 3.4 TLS records opened from pieces
- [ ] 3.5 A frame in chunks sent on without a copy of ours: whether the area can also be registered as fixed buffers, tried on the Spark; `send_lent` for such a frame by what that shows
- [ ] 3.6 Every row of the ownership and exhaustion tables reached by a test; allocation guard over the mode
- [ ] 3.7 `tcp_uring`: `--rx zc`, and the real-kernel self-check run with it where the host can
- [ ] 3.8 Review of group 3

## 4. Zero-copy sends over a card
- [ ] 4.1 Where a zero-copy send of 64 KiB waits between its completion and its notification, traced on the Spark pair
- [ ] 4.2 The fix it points to; defaults for segment length and sends in flight
- [ ] 4.3 The wire matrix of milestone 30 again, five runs a cell: no frame size slower with zero-copy than without, by the medians
- [ ] 4.4 Review of group 4

## 5. Measurement and the decision on AF_XDP
- [ ] 5.1 Spark pair, both interfaces: GB/s a receiving core and cores at the full link, counting the queues' cores, `Ring`, `Direct` and `ZeroCopy`, frames of 324 bytes, 64 KiB and 1 MiB, forwarded unread, read in place, and held for a log write; three runs a cell
- [ ] 5.2 Results in `bench/results/`; card settings and the support matrix in `docs/tcp.md`
- [ ] 5.3 `add-tcp-af-xdp-backend` updated with what is left for it to win, or closed
- [ ] 5.4 Review of group 5
