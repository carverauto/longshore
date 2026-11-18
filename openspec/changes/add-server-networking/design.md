## Context
`Accept` names the slot the connection goes into and completes once. `Recv`
names the buffer it fills, so a connection waiting for bytes holds a buffer.
`Completion` is a token and a 32-bit result. The ring backend frees an
operation's slot at its first completion, with zero-copy send as the one
case that waits for a second.

Sockets in the ring are registered files with no descriptor, so an option
cannot be set on them with `setsockopt(2)`. The command that sets options
through the ring arrived in Linux 6.7. The measuring connect already takes
another route: `socket(2)`, register, connect, and the descriptor is closed.

`Stream` is in `longshore_http_client/src/stream.rs` and depends on
`longshore_tls`, which depends on AWS-LC.

## Goals / Non-Goals
- Goals: an idle connection costs a socket slot and nothing else; one
  submission arms an accept or a receive for many completions; socket
  options and Unix-domain sockets on Linux 6.1; one stream type for
  clients and servers; a plain build without a C toolchain
- Non-Goals: HTTP/2; a server framework; zero-copy receive; kernel TLS;
  sending from the buffer ring

## Decisions
- `Completion` gains a flag, "more will follow for this token", and the
  number of the buffer it filled. An operation that completes once never
  sets the flag. The contract's "one final completion" is the completion
  without the flag.
- The accept that stays armed takes its sockets from a range of slots that
  `Limits` reserves for it. The backend picks a free slot and reports it
  as the result. Slots the caller assigns are outside that range.
  When the range is full the accept ends with a final completion carrying
  the no-space error, and the caller arms it again after closing a socket.
- The receive that stays armed names a socket and takes buffers from the
  buffer ring. Each completion carries the buffer and the byte count. The
  caller reads the bytes in place and gives the buffer back with a call
  that submits nothing. When the ring is empty the receive ends with a
  final completion carrying the no-buffers error. A close or a cancel ends
  it with a final completion too.
- The buffer ring is a fourth class of pool buffers, sized in `Limits`.
- No-delay and keep-alive are a setting of the backend, applied to every
  TCP socket it creates. A listener is created with a descriptor, the
  options are set on it, and accepted sockets inherit them. A connecting
  socket goes the descriptor route the measuring connect uses.
  Alternative considered: the ring's socket-option command. Rejected
  because it needs Linux 6.7.
- A Unix-domain listener is bound by path when the backend is set up, as a
  TCP listener is. A connect takes its path from a pool buffer.
- In the engine, an armed operation stays in the pending table and is
  attempted again after each completion, until one is final. Both worlds
  get these operations from that one rule.
- `longshore_stream` holds `Stream` and its record handling and depends on
  `longshore_common`, and on `longshore_tls` under the `tls` feature. The
  HTTP client depends on it. Without the feature a `Stream` is plain and
  asking for TLS is an error when the client is built.
  Alternative considered: leave `Stream` in the HTTP client and have
  servers depend on that crate. Rejected because a server would then carry
  an HTTP client to open a socket.

## Risks / Trade-offs
- A caller that never gives buffers back starves every connection. → The
  no-buffers completion names the cause, and the shared tests cover it.
- Option inheritance on accept is the kernel's behaviour, not a documented
  promise of the ring. → A real-kernel check reads the options back from
  an accepted socket on 6.1 and on a current kernel.
- Moving `Stream` touches the S3 path. → Figures before and after.

## Migration Plan
1. `longshore_stream` and the `tls` feature, with no change in behaviour.
2. `Completion` fields; every existing operation leaves them clear.
3. The armed accept and receive, the buffer ring.
4. Socket options, Unix-domain sockets.

## Open Questions
None.
