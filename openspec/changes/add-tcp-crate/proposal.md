# Change: A TCP crate, longshore_tcp: design, API and io_uring backend

## Why
longshore speaks TCP only as the transport under its HTTP client and server.
Messaging, replication and database protocols run straight over TCP, and
UltraGraphDB's networking (issue #56; its binary protocol, role links and
control plane) is the last part of its I/O stack still on compio. A
protocol-neutral TCP layer with longshore's rules (allocation at build,
no copy of payload, one thread per core, protocol code without I/O, timers
chosen by the caller) closes that gap.

This is the first of three changes:
1. **This change**: the design, the API, and a backend over the `Io` trait
   that uses the kernel's TCP through io_uring (and, through the same
   trait, the portable backend and the simulator).
2. `add-tcp-simulation-and-benchmarks`: deterministic simulation with
   faults, property tests and fuzzing, and the benchmarks that pick the
   defaults.
3. `add-tcp-af-xdp-backend`: kernel bypass through AF_XDP with a user-space
   TCP stack behind the same API, tuned on 200 and 300 Gb/s NICs.

## What Changes
- New crate `longshore_tcp`, re-exported by `longshore` as `tcp`:
  - `Net<T: Transport>`: a fixed set of connections and listeners driven
    from the caller's loop. It borrows the backend on each call and never
    waits: the caller routes completions to it (`on_completion`), calls
    `poll(now, ..)`, and reads `next_deadline()`, so file I/O and other
    clients share the same ring and thread.
  - `Transport`: the backend seam. It names no io_uring type, so a kernel
    bypass backend fits behind it (change 3). `KernelTransport<IO: Io>` is
    the first: io_uring on Linux, the portable backend and the simulator
    through the same `Io` trait.
  - Handles with a generation (`Conn`, `Listener`): a stale handle is
    refused, never routed to a reused slot.
  - Framing: raw bytes, or frames with a fixed-size header carrying the
    length (a 4-byte prefix, or UltraGraphDB's 256-byte header with the
    size at offset 40), up to a maximum frame size.
  - Received frames lent in place; frames in frame memory held until
    released, ring-lent frames until the next `poll` unless held; a
    per-connection budget of held bytes pauses reading (backpressure
    without refusal).
  - Frames serialised by the caller straight into registered send memory;
    queued frames coalesced into one send; zero-copy send above a size.
  - Connect with timeout; reconnect with exponential backoff and jitter;
    listen with `SO_REUSEPORT` per core, bind retry, a connection limit
    with a refusal slot, and accept backoff on `EMFILE`; orderly close,
    reset, half-close and drain; idle and handshake deadlines.
  - TCP and Unix-domain sockets; TLS per endpoint with any of the TLS
    providers.
- `longshore_common::io` gains what UltraGraphDB needs and the `Io` trait
  lacks: shutting down one direction (`Op::Shutdown`), the peer address of
  an accepted connection, keepalive timings, and `SO_REUSEPORT` and the
  backlog when listening. `longshore_stream::Stream` gains a receive path
  for buffer-ring buffers, so TLS works with armed receives. All within Linux 6.1. **BREAKING** for code
  outside longshore that implements `Io` or matches `Op` exhaustively:
  `Op::Shutdown` is a new variant.
- Tests in this change: framing and handle units, the API against
  `UringIo` (real kernel) and `PosixIo` over loopback and Unix sockets, and
  an allocation check. Simulation with faults and benchmarks are change 2.

## Impact
- Affected specs: `tcp-connections` (ADDED), `server-networking` (ADDED
  requirements: half-close, peer address, keepalive timing, shared port,
  stream receives from the buffer ring)
- Affected code: new `crates/longshore_tcp`; `longshore_common::io`
  (`Op`, `UringIo`, `PosixIo`, `SimIo`); `longshore_stream`; `longshore` facade; README,
  `docs/`, `openspec/project.md` (ten crates)
- Breaking only as above; the new `Io` methods have defaults
