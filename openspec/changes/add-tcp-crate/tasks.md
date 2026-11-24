## 1. Io additions (longshore_common)
- [x] 1.1 `Op::Shutdown { sock, how }` on `UringIo` (`IORING_OP_SHUTDOWN`), `PosixIo` and `SimIo`, with contract tests
- [x] 1.2 `Op::Accept` records the peer address; `Io::peer_addr(sock)` on all three backends
- [x] 1.3 Keepalive idle, interval and count; `SO_REUSEPORT` and backlog on listen
- [x] 1.4 `longshore_stream`: `Stream::on_ring_recv` for plain and TLS (records opened into the destination, a record continuing across ring buffers), with simulator tests
- [ ] 1.5 `Extents` (hold counts and generations over the aligned buffer class) and `Turn` (token classes, storage-first dispatch) in `longshore_common`, with tests
- [ ] 1.6 Real-kernel checks in `longshore-bench` for the above

## 2. Crate and API
- [ ] 2.1 `crates/longshore_tcp` (workspace member, lints, license headers, README); `longshore::tcp`
- [ ] 2.2 `Config` and its builder (validated: socket-slot, token and buffer ranges; `Config::limits(base)`), `Net<T: Transport>` borrowing `T::Driver` (`on_completion`, `poll`, `next_deadline`), `Conn`/`Listener` handles with generations, `Event`, errors
- [ ] 2.3 `Transport` trait with an associated `Driver`: slots, lent receive buffers, reserved send space, transport events; no backend types
- [ ] 2.4 Framing: `Bytes` and `Header { .. }`, with the length bounds
- [ ] 2.5 Frames: `FrameId`, `frame`, `release`, `hold` (ring-lent frames end at the next `poll` unless held), held-bytes budget pausing reads, re-arming after an empty ring
- [ ] 2.6 Send queues: segments from a shared pool, span queues, `send_lent`, `send_frame`/`send_bytes`, coalescing, zero-copy threshold, `Full` and `Writable`
- [ ] 2.7 Deadlines: fixed table, `next_deadline`, connect/handshake/idle timeouts, a limit on segments sent per `poll`
- [ ] 2.8 Connect with reconnect policy (backoff, seeded jitter, `Down`/`Up`); listen with reuse port, bind retry, connection limit and refusal slot, accept backoff, `peer_addr` (single accepts) and `peer(conn)`
- [ ] 2.9 Close, `shutdown_write`, `drain`; closing reasons

## 3. Kernel transport
- [ ] 3.1 `KernelTransport<IO: Io>`: connect, listen, accept (armed), `Rx::Ring` (armed receive, frames lent in place, spanning frames assembled) and `Rx::Direct` (header then body into frame memory)
- [ ] 3.2 Sends from segments, one in flight per connection; zero-copy above the threshold on TCP without TLS only
- [ ] 3.3 Unix-domain sockets; TLS through `longshore_stream::Stream` with any provider, in both receive modes

## 4. Tests
- [ ] 4.1 Units: framing (both kinds, bounds), handle generations, backoff, deadline table, send coalescing
- [ ] 4.2 API over `UringIo` (real kernel) and `PosixIo`: TCP and Unix, with and without TLS; every scenario of the spec deltas
- [ ] 4.3 Allocation check under `alloc_guard`
- [ ] 4.4 File writes and syncs and a `Net` on one `Io`, completions routed by token
- [ ] 4.5 Receive, direct-I/O write and three lent sends of one frame without a copy; reuse only at zero holds
- [ ] 4.6 The same exchange on `UringIo`, `PosixIo` and `SimIo` gives the same frames

## 5. Documentation
- [ ] 5.1 `docs/tcp.md` (the API, the two receive modes, sizing), README section, crate docs with an example
- [ ] 5.2 `openspec/project.md`: ten crates, `longshore_tcp`
