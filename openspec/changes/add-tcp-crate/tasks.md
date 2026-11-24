## 1. Io additions (longshore_common)
- [ ] 1.1 `Op::Shutdown { sock, how }` on `UringIo` (`IORING_OP_SHUTDOWN`), `PosixIo` and `SimIo`, with contract tests
- [ ] 1.2 Peer address of an accepted slot on all three backends
- [ ] 1.3 Keepalive idle, interval and count; `SO_REUSEPORT` and backlog on listen
- [ ] 1.4 Real-kernel checks in `longshore-bench` for the above

## 2. Crate and API
- [ ] 2.1 `crates/longshore_tcp` (workspace member, lints, license headers, README); `longshore::tcp`
- [ ] 2.2 `Config` and its builder (validated), `Net<T: Transport>`, `Conn`/`Listener` handles with generations, `Event`, errors
- [ ] 2.3 `Transport` trait: slots, lent receive buffers, reserved send space, transport events; no backend types
- [ ] 2.4 Framing: `Bytes` and `Header { .. }`, with the length bounds
- [ ] 2.5 Held frames: `FrameId`, `frame`, `release`, held-bytes budget pausing reads
- [ ] 2.6 Send queues: segments from a shared pool, `send_frame`/`send_bytes`, coalescing, zero-copy threshold, `Full` and `Writable`
- [ ] 2.7 Deadlines: fixed table, `next_deadline`, connect/handshake/idle timeouts, per-`poll` completion limit
- [ ] 2.8 Connect with reconnect policy (backoff, seeded jitter, `Down`/`Up`); listen with reuse port, bind retry, connection limit and refusal slot, accept backoff
- [ ] 2.9 Close, `shutdown_write`, `drain`; closing reasons

## 3. Kernel transport
- [ ] 3.1 `KernelTransport<IO: Io>`: connect, listen, accept (armed), `Rx::Ring` (armed receive, frames lent in place, spanning frames assembled) and `Rx::Direct` (header then body into frame memory)
- [ ] 3.2 Sends from segments, zero-copy above the threshold
- [ ] 3.3 Unix-domain sockets; TLS through `longshore_stream::Stream` with any provider

## 4. Tests
- [ ] 4.1 Units: framing (both kinds, bounds), handle generations, backoff, deadline table, send coalescing
- [ ] 4.2 API over `UringIo` (real kernel) and `PosixIo`: TCP and Unix, with and without TLS; every scenario of the spec deltas
- [ ] 4.3 Allocation check under `alloc_guard`
- [ ] 4.4 The same exchange on `UringIo`, `PosixIo` and `SimIo` gives the same frames

## 5. Documentation
- [ ] 5.1 `docs/tcp.md` (the API, the two receive modes, sizing), README section, crate docs with an example
- [ ] 5.2 `openspec/project.md`: ten crates, `longshore_tcp`
