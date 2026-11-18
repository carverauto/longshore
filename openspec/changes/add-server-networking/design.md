## Context
`Accept` names the slot the connection goes into and completes once. `Recv`
names the buffer it fills, so a connection waiting for bytes holds a buffer.
`Completion` is a token and a 32-bit result, built by a literal in eight
places. The ring backend frees an operation's slot at its first completion;
zero-copy send is the one case that waits for a second, and is recognised
by the kernel's "more" flag alone, whatever the slot is doing.

Each operation yields at most two completions today and the completion
queue is twice the submission queue, so it cannot fill. No code looks at
the kernel's overflow state.

Sockets in the ring are registered files with no descriptor. A listener is
made with a descriptor, registered, and the descriptor closed. The
measuring connect makes its socket the same way, and falls back to a
socket made in the ring when that fails.

`Stream` is in `longshore_http_client/src/stream.rs`. It depends only on
`longshore_common` and `longshore_tls`, and only the HTTP client and its
test server use it. It receives into a buffer of its own and keeps two
record buffers for each TLS connection. `longshore_s3` signs requests with
`ring`, which compiles C.

Facts about Linux 6.1 this design rests on, read from the 6.1 source:
- An accept that stays armed can place connections only in slots the
  kernel picks. The picking can be confined to a range of the table. When
  the range is full, the kernel closes the new connection and ends the
  accept with the table-full error.
- A receive that stays armed must take its buffers from a buffer ring, one
  buffer per completion. The ring's memory is the caller's and must be
  page-aligned. An empty ring ends the receive with the no-buffers error.
  The buffer is named in the completion's flags.
- When the completion queue is full, an armed operation ends. Its last
  completion carries what it had: a connection or bytes. Later kernels
  queue instead, or end it under other conditions.
- Accepted sockets inherit no-delay and keep-alive from the listener.
- The command that sets socket options through the ring is not in 6.1.

## Goals / Non-Goals
- Goals: an idle plain connection costs a socket slot and an operation
  slot and no buffer; one submission arms an accept or a receive for many
  completions; socket options and Unix-domain sockets on Linux 6.1; one
  stream type for clients and servers; a plain build without AWS-LC
- Non-Goals: HTTP/2; a server framework; zero-copy receive; kernel TLS;
  sending from the buffer ring; a TLS stream without standing buffers; a
  build of the S3 crate without a C compiler, which `ring` prevents

## Decisions

### Completions
- `Completion` gains a flag, "more will follow for this token", and the
  buffer it filled, if any. It gets a constructor for the common case, and
  the eight literals use it. An operation that completes once never sets
  the flag.
- An armed operation may end at any time. Its last completion is the one
  without the flag, and it may carry a result like any other: an accepted
  socket, bytes in a buffer, zero, or an error. A caller handles the
  result first and then decides whether to arm again. This one rule covers
  every kernel's behaviour.
- A completion carries a buffer only when it carries bytes. When the
  kernel hands back a buffer with nothing in it, the backend returns it to
  the ring itself.
- The simulator's trace hash takes in the new fields only when they are
  set, so no existing trace changes.

### The ring backend
- A slot's state says whether its operation is armed. Completions are
  dispatched on that state before the "more" flag is read, and an armed
  slot is freed only by its last completion.
- The completion queue's size is set explicitly: room for two completions
  per operation slot as today, plus one for every buffer of the buffer
  ring and every slot of the accept range. Setup checks that the kernel
  keeps completions it cannot queue rather than dropping them, and fails
  if it does not.
- Closing a socket cancels the operations in flight on it, armed ones
  included, as the contract in `add-portable-io-backend` requires. For a
  connected stream socket the backend submits a shutdown and then the
  close. The peer therefore sees the end even if the kernel defers
  releasing the slot.
- `ops_max` counts armed operations like any other.

### Armed accept
- The accept range is a run of socket slots that `Limits` reserves. The
  backend confines the kernel's choice to it. The completion's result is
  the slot. Slots outside the range are the caller's to assign, as today.
- When the range is full the new connection is dropped, and the accept
  ends with the table-full error. The simulator does the same: the
  connecting side sees a reset. A server keeps its range at least as large
  as the connections it admits.

### Armed receive and the buffer ring
- The buffer ring is a fourth class of pool buffers, at most 32,768, sized
  in `Limits`. Its index memory is mapped page-aligned when the backend is
  built and is leaked with the pool if the backend cannot stop cleanly.
- An armed receive names a socket. Each completion with bytes names the
  buffer and the count. The caller reads the bytes in place and gives the
  buffer back with a new `Io` method that submits nothing. The method has
  no default, so every implementation of `Io`, the test wrapper in
  `longshore_s3` among them, must provide it.
- When the ring is empty the receive ends with the no-buffers error. The
  bytes stay in the socket, and the caller arms again once it has given
  buffers back.

### Options and Unix-domain sockets
- No-delay and keep-alive are a setting of the backend. They are set on a
  TCP listener's descriptor before it is closed, and accepted sockets
  inherit them. When the setting is on, a connecting TCP socket is made
  with a descriptor, as the measuring connect's is, and the options are
  set on it. If that socket cannot be made, the connect completes with the
  error; it does not fall back to a socket without the options.
  Alternative considered: the ring's socket-option command. Rejected
  because it needs Linux 6.7.
- A Unix-domain listener is bound by path with a setup call, as a TCP
  listener is by address. A connect takes its path from a pool buffer, as
  bytes that end in a zero byte and hold no other, and the backend copies it
  into the slot's address storage when the connect is submitted, since the
  kernel needs the address after the socket is made.
- Socket options are not applied to Unix-domain sockets.

### Simulator and engine
- In the engine an armed operation stays in the pending table. Each step
  delivers at most one completion for it, takes that completion into the
  trace, and visits it again in the next step.
- The virtual world keeps a free list for the buffer ring and copies from
  a socket's bytes into a buffer when a receive is armed. Its sockets gain
  an address that is an IP address or a path.
- Tests in the simulator use tens of idle connections, since each
  simulated socket holds its own bytes. The thousand-connection check runs
  on a real kernel.

### Stream and the TLS feature
- `longshore_stream` holds `Stream` and its record and handshake handling.
  It depends on `longshore_common`, and on `longshore_tls` under the `tls`
  feature. The HTTP client depends on it. `Stream` behaves as today: it
  receives into its own buffer and keeps two record buffers per TLS
  connection. A server makes one for each accepted slot.
  Alternative considered: leave `Stream` in the HTTP client. Rejected
  because a server would then carry an HTTP client to open a socket.
- The error type for TLS is a plain enum that exists with or without the
  feature, so code that names it needs no condition.
- The workspace's dependency entries turn default features off, so each
  crate can pass the `tls` feature on. `longshore`, `longshore_s3`,
  `longshore_http_client` and `longshore_stream` each have it, on by
  default.
- Without the feature a `Stream` is plain. A client built with TLS asked
  for fails with a setting error that names the feature; code that names
  TLS setup types does not compile.
- What the feature removes is `longshore_tls` and AWS-LC from the
  dependencies of `longshore` itself. Test and benchmark programs still
  use TLS, and `ring` still needs a C compiler for the S3 crate.

## Risks / Trade-offs
- A caller that never gives buffers back starves every connection. → The
  no-buffers completion names the cause, and the shared tests cover it.
- A shutdown before each close is one more submission per connection. →
  It is not on the data path, and the real-kernel check measures a
  connection's whole life.
- Moving `Stream` touches the S3 path. → Figures before and after.

## Migration Plan
1. `longshore_stream` and the `tls` feature, with no change in behaviour.
2. `Completion` fields and constructor; every existing operation leaves
   them clear.
3. Queue sizing, slot states, and shutdown before close in the ring.
4. The armed accept and receive, the buffer ring.
5. Socket options, Unix-domain sockets.

## Open Questions
None.
