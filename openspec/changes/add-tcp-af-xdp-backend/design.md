## Context
After change 2, `longshore_tcp` runs kernel TCP through io_uring with
measured defaults, and a packet-level simulated device exists. The hosts
this targets have 200 and 300 Gb/s NICs (25 and 37.5 GB/s). Measured so
far on kernel TCP, one core: HTTPS upload 7.2 GB/s and plain 12 GB/s on an
M3 Max VM; on download the kernel's receive copy dominates.

## Goals / Non-Goals
- Goals: line rate on a 200 Gb/s NIC and as close as the hosts allow on
  300 Gb/s, with fewer cores and lower p99 latency than kernel TCP; one
  move per payload byte on receive and none on transmit; the change 1 API
  unchanged.
- Non-Goals: Unix-domain sockets over AF_XDP (they stay on the kernel
  transport); sharing a connection between kernel TCP and the user-space
  stack; DPDK.

## Decisions
- **AF_XDP, not DPDK**: in-kernel drivers, no NIC unbinding, other traffic
  stays with the kernel through `XDP_PASS`.
- **One socket per queue per core**: the core that owns a queue owns its
  rings and its connections; no locks, as everywhere in longshore.
- **The pool is the UMEM**: each core's `Io` pool, on huge pages, is
  registered as io_uring buffers and as that core's UMEM. Receive chunks
  come from a class of the pool; extents (change 1) are transmitted in
  place through multi-buffer descriptors. Nothing in the `Net` API or in
  the caller changes: frames are still extents with hold counts.
- **One move on receive**: payload is moved once from receive chunks into
  its extent, where it is block-aligned for the log and contiguous for the
  caller; the caller's checksum can run in the same pass. Receive is thus
  not zero-copy and does not need to be: it costs what kernel TCP's one
  copy costs, without the kernel's per-packet work.
- **One wait point**: idle, the thread sleeps in its ring with a multishot
  poll (`Op::PollFd`) armed on the AF_XDP socket; busy, it spins on the
  queue and ticks the ring without waiting.
- **Driver zero-copy mode by default, copy mode on request**: this is
  AF_XDP's own setting (whether the driver copies packets between the NIC
  and the UMEM), separate from the one move above. Copy mode exists for
  development hosts and VMs; production refuses rather than degrade
  silently.
- **Flow placement**: on connect, choose a source port whose RSS hash lands
  on the owning core's queue (Toeplitz hash computed with the NIC's key);
  for listeners, flow rules or RSS spreading with one listener per queue.
- **Neighbours and routes from the kernel**: read through netlink at build
  and on change; the user-space stack does not run ARP/NDP itself for
  addresses the kernel owns.
- **TLS**: `longshore_tls::record` seals into transmit frames and opens in
  place, as in the kernel transport, so every TLS provider works.
- **User-space TCP: decided by a spike.** Candidates: smoltcp, extended
  (window scaling, SACK, large windows, DCTCP/CUBIC, batched transmission),
  or a stack written for this. Criteria: interoperability with Linux TCP
  under loss, GB/s per core and p99 latency in the packet simulator and on
  the NICs, and the size of the changes needed. A dependency is adopted
  only with a benchmark in `bench/results/` (principle 7).
- **zcrx in the same study**: io_uring zero-copy receive keeps kernel TCP
  and removes the receive copy; if it gets close to AF_XDP on these NICs,
  it becomes an option of the kernel transport on kernels that have it.

## Method
The review of `add-tcp-crate` found about two dozen faults, nearly all of
two kinds: events in an awkward order (a close racing with a completion, a
slot taken again too early) and a pool empty at an awkward moment. The
`design.md` of `add-tcp-simulation-and-benchmarks` records them and the
method both remaining changes follow; in short: a table for every state
machine and the invariants before its code, an answer for every empty
pool, adversarial scenarios for every requirement, a model test with the
first code, and a review at the end of each task group. A user-space TCP
stack has far more states, timers and pools than `Net`, so here the tables
below are the first task, and no code for a machine is written before its
table has been reviewed.

`Net` itself is unchanged, and so are its state tables. `XdpTransport`
must pass the transport conformance suite of change 2 from its first
commit: that suite is what keeps the faults of change 1's seam (a report
after a slot was taken again, a close reported twice or never) from coming
back in a second transport.

## The TCP connection state table
The stack's connection machine, per RFC 9293, with what this transport
reports to `Net`. "ACK ok" is an acknowledgement of something sent and not
yet acknowledged. A segment is "in window" by its sequence number.
"challenge" is the ACK of RFC 5961 that makes a spoofed reset or SYN
harmless. Every cell also restarts or stops the retransmission timer as
the standard says; that is left out here.

| State | `connect` | SYN | SYN+ACK | ACK ok | data in window | FIN in window | RST, exact / in window | out of window | `shutdown_write` | `close` | retransmission timer |
|---|---|---|---|---|---|---|---|---|---|---|---|
| CLOSED (slot free) | send SYN → SYN-SENT | no listener: RST; listener: → SYN-RCVD in an accept slot, or drop when none is free | RST | RST | RST | RST | drop | RST | — (1) | — (1) | — |
| SYN-SENT | — (2) | simultaneous open: SYN+ACK → SYN-RCVD | ACK → ESTABLISHED, report `Connected` | drop | drop | drop | exact (acks our SYN): report `Connected` failed, → CLOSED | drop | — (3) | RST not needed; → CLOSED, report `Closed` | resend SYN with backoff; the connect timeout is `Net`'s |
| SYN-RCVD | — (2) | resend SYN+ACK | drop | → ESTABLISHED, report `Accepted` | queue, as ESTABLISHED once acked | → CLOSE-WAIT after the ACK | exact: → CLOSED, slot back, nothing reported | challenge | — (3) | — (3) | resend SYN+ACK; give up after the retry limit, slot back, nothing reported |
| ESTABLISHED | — (2) | challenge | challenge | advance, release what was acknowledged, report `Sent` | move to frame memory or queue out of order; ACK; report data | ACK → CLOSE-WAIT, report end of stream after all data | exact: report a reset, → CLOSED after `close`; in window: challenge | challenge | FIN after queued data → FIN-WAIT-1 | (4) | resend, congestion control reacts |
| FIN-WAIT-1 | — (2) | challenge | challenge | of our FIN: → FIN-WAIT-2, report `ShutDown` | as ESTABLISHED | ACK → CLOSING, report end of stream | as ESTABLISHED | challenge | — (5) | (4) | resend |
| FIN-WAIT-2 | — (2) | challenge | challenge | ignore | as ESTABLISHED | ACK → TIME-WAIT, report end of stream | as ESTABLISHED | challenge | — (5) | (4) | — |
| CLOSE-WAIT | — (2) | challenge | challenge | advance, report `Sent` | drop, ACK (6) | resend the ACK | as ESTABLISHED | challenge | FIN after queued data → LAST-ACK | (4) | resend |
| CLOSING | — (2) | challenge | challenge | of our FIN: → TIME-WAIT, report `ShutDown` | drop, ACK | resend the ACK | exact: → CLOSED | challenge | — (5) | (4) | resend |
| LAST-ACK | — (2) | challenge | challenge | of our FIN: → CLOSED, report `ShutDown` | drop, ACK | resend the ACK | exact: → CLOSED | challenge | — (5) | (4) | resend |
| TIME-WAIT (linger table) | — (7) | new connection if its sequence number is beyond the old one's, else challenge | challenge | ignore | drop, ACK | resend the ACK, restart the wait | exact: remove | challenge | — (7) | — (7) | the wait is over: remove |

1. `Net` holds no connection in a free slot.
2. `connect` is called once per connection, on a free slot.
3. `Net` calls neither before the connection is reported up.
4. `close` ends the slot at once, as `Net` requires (`Closed` is reported
   in the same turn and nothing after it). With data unsent or received
   and not yet handed to `Net`: RST. Otherwise: FIN, and what is left of
   the connection (addresses, ports, sequence numbers, one timer) moves to
   a fixed **linger table** that answers the peer until the close
   handshake and the wait are over. A full linger table sends RST instead.
   The slot is thus free for a new connection while the old one's packets
   are still answered, and they are matched by address, port and sequence
   number against the linger table first, never against the slot's new
   occupant.
5. `Net` shuts down once.
6. After the peer's FIN nothing more is acceptable from it.
7. The linger table has no slot and no handle; `Net` cannot name it.

## The UMEM chunk ownership table
A chunk is in exactly one place. A chunk that is in none, or in two, is
the fault that corrupts a packet in flight.

| Where a chunk is | Who may touch it | It leaves when | To |
|---|---|---|---|
| Free list | the transport | the fill ring has room, or a transmit needs a header chunk | Fill ring; Transmit staging |
| Fill ring | the kernel | the NIC receives a packet into it | Receive ring |
| Receive ring | the transport, once reaped | parsed: payload moved to frame memory, or held out of order, or the packet dropped | Free list; Reassembly |
| Reassembly (out of order) | the transport | the gap before it is filled and its payload moved; or it is dropped under pressure | Free list |
| Transmit staging (headers) | the transport | the transmit ring has room | Transmit ring |
| Transmit ring | the kernel | the NIC has sent it | Completion ring |
| Completion ring | the transport, once reaped | reaped | Free list |

Payload is not in chunks on the way out: a transmitted frame is fragments
that point into its extent or segment. That memory is **held until the
bytes are acknowledged**, not until the transmit completes, because a
retransmission sends from it again. `Net` learns of a send's end
(`Sent`) at the acknowledgement for the same reason.

## The queue socket state table

| State | built | packet or completion | nothing in flight, nothing due | wake in the ring | link down, program detached | shut down |
|---|---|---|---|---|---|---|
| Unbuilt | check the driver, privileges and kernel; refuse with what is missing, or register the UMEM, map the rings, attach the program → Running | — | — | — | — | — |
| Running (spinning) | — | process a bounded batch | arm the poll in the ring, set need-wakeup → Idle | — | → Failed | detach, → Unbuilt |
| Idle (armed) | — | — (it wakes first) | stay | → Running | → Failed | detach, → Unbuilt |
| Failed | — | drop | — | — | stay | detach, → Unbuilt |

In `Failed` every connection of the queue is reported to `Net` as reset,
once. The transport does not fall back to the kernel's stack by itself.

## Invariants
Checked after every step of the packet-level model test.

- **X1** Every chunk is in exactly one row of the ownership table; the
  counts of the rows add up to the UMEM.
- **X2** Memory a segment was sent from stays held until that segment is
  acknowledged or its connection has ended.
- **X3** Sequence numbers: acknowledged ≤ sent ≤ acknowledged + the peer's
  window; nothing beyond what was received in order is acknowledged.
- **X4** Bytes reach `Net` once and in order, whatever was lost,
  duplicated or reordered on the way.
- **X5** A segment that is sent and not acknowledged has a timer; a
  connection with nothing unacknowledged has none.
- **X6** A reset or SYN is acted on only as RFC 5961 allows.
- **X7** A packet for a connection the queue does not own is counted and
  dropped, never processed.
- **X8** The thread sleeps only with nothing queued to transmit, no
  completion unreaped, no timer due, and the poll armed.
- **X9** A packet is matched against the linger table before the slots; a
  slot's new connection never receives its predecessor's packets.
- **X10** The transport conformance suite of change 2 passes.
- **X11** Nothing allocates after build.

## Exhaustion

| Pool | Who takes | When it is empty | What wakes the taker |
|---|---|---|---|
| Free chunks for the fill ring | the receive path | the NIC drops; TCP recovers the loss | chunks returned from receive, reassembly and completion: the fill ring is topped up in the same turn |
| Transmit ring | every segment | the segment stays queued on its connection | completions reaped at the start of every turn |
| Completion ring | the kernel | transmits stall | reaped every turn before anything is transmitted |
| Header chunks | every segment | as the transmit ring | a completion |
| Reassembly | out-of-order segments | the segment is dropped; SACK tells the peer what is held | the gap filled |
| Frame memory | the one move on receive | the receive window closes to zero | `release`: a window update is sent in the same turn, and the peer's zero-window probe is answered |
| Send window, congestion window | every send | `Net` gets `Full` through its queue; the bytes stay queued | an acknowledgement or a window update |
| Retransmission queue | every sent segment | bounded by the send window: cannot be exceeded | — |
| Accept slots | a SYN for a listener | the SYN is dropped (the peer retries); no half-open state is kept | a slot freed |
| Linger table | `close` with nothing unsent | RST instead of FIN | an entry's wait ending |
| Neighbour entry | the first packet to a peer | the connect waits; one packet is kept | the kernel's neighbour table changing (netlink) |

## Testing
- **Differential.** The stack against Linux TCP as the peer, over the
  packet device of change 2 and over veth: the same bytes both ways under
  loss, reordering, duplication and delay, and the same reaction to a
  reset, a zero window and a simultaneous close.
- **Model.** Seeded random operations of `Net`'s caller on both ends and
  seeded packet faults between them, with the smallest UMEM, rings and
  windows; X1 to X11 and `Net`'s own invariants checked after every step;
  a failing seed replayed by one command.
- **Conformance.** Change 2's suite, from the first commit of the
  transport.
- **Fuzzing** of the packet parser and of the model's steps.

## Risks / Trade-offs
- A user-space TCP stack is a large surface → interoperability tests
  against Linux peers with loss and reordering, the packet simulator from
  change 2, and fuzzing of the packet parser.
- Driver support for AF_XDP zero copy and multi-buffer varies → a support
  matrix in `docs/tcp.md`, and refusal with the missing piece named.
- Privileges (`CAP_NET_ADMIN`, `CAP_BPF`, `CAP_NET_RAW`) and exclusive NIC
  queues are deployment constraints → documented, checked at build.

## Migration Plan
Opt-in per `Net` by choosing the transport; programs keep the kernel
transport unless they choose otherwise.

## Open Questions
- The size of the linger table, and how long an entry waits (twice the
  longest segment lifetime is 60 s to 4 min by the standard; a data centre
  can use far less).
- smoltcp extended, or a stack written in tree (settled by the spike).
- Huge page size (2 MiB or 1 GiB).
- Whether transmit fragments may point anywhere inside an extent on every
  target driver (to be verified on the NICs before the pool layout is
  fixed).
- Whether zcrx makes AF_XDP unnecessary on some of the target NICs.
