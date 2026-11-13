## Context
Sockets are created by `io_uring` directly into the registered file table,
so the process holds no descriptor for them. The negotiated segment size is
a TCP option (`TCP_MAXSEG`), and `io_uring`'s socket command reads options
at the socket level only, on any kernel. Reading it needs a descriptor.

## Goals / Non-Goals
- Goals: the same behaviour on every supported kernel (Linux 6.1 and
  later); no cost while data moves; testable in the simulator
- Non-Goals: changing the segment size a path allows, choosing between
  addresses by latency or load, and anything for stores whose addresses all
  behave alike

## Decisions
- When measuring is on, the backend opens the socket with `socket(2)`,
  registers it in its slot, and connects through the ring as before. When
  the connect completes it reads `TCP_MAXSEG` with `getsockopt(2)` and
  closes its descriptor. The registered table keeps the socket. That is
  four system calls per new connection in place of one submission.
- A successful `Op::Connect` completes with the connection's segment size
  and the largest segment its path could carry (the path's MTU less the
  headers), packed into the result, and with 0 when it was not measured.
  No new operation is added.
- The HTTP client reports the address and size of each opened connection to
  its owner, the way it reports a failed connect.
- The resolver keeps the last size seen per address. An address with no
  measurement is handed out like any other, so every address gets measured.
  An address counts as large when its size is at least half the largest
  seen and at least a quarter of what the path could carry. Lookups skip
  the others once a large one is known. If that would leave fewer
  addresses than a configured minimum, the largest of the skipped ones are
  kept.
- The store stops reusing connections that measured below that floor. Each
  finishes its request and is replaced the next time it would be picked.
- When every address of a host is measured and all are small for the path,
  the resolver asks again one answer timeout after its last cycle, and
  keeps doing so until an answer has a large one. The store then opens one
  connection to each new address to measure it. On a path that carries no
  more than the addresses give, such as plain Ethernet to a store outside
  the network, nothing is small and none of this happens.
- The setting can be changed while the store runs.
- A refresh keeps the size of an address that is still in the answer and
  forgets the others.
- Off by default. `s3_uring` turns it on with `S3_PREFER_LARGE_SEGMENTS=1`.

## Risks / Trade-offs
- Fewer addresses carry the load. A store may rate-limit or shed load per
  address, which the minimum number of addresses limits and does not remove.
- An address's size can change between measurements. Each new connection
  measures again, so a stale value lasts until the next connection to it.
- A name that only ever answers with small-segment addresses costs one
  DNS query per answer timeout for as long as that lasts.
- A client that keeps a single connection learns one address at a time.
  With the path limit known it still moves off a small one.

## Open Questions
- Whether an S3 gateway endpoint makes every address take large segments.
  Creating one in the benchmark VPC needs the owner's go-ahead.
