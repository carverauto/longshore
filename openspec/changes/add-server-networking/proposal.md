## Why
longshore's networking was written for a client: it connects, and it
accepts only in its test server, one connection per operation. A server
with thousands of connections needs more. UltraGraphDB keeps a buffer of
1 MiB for every connection whether it is idle or not, copies each request
once on its way to the core, and sets its socket options through another
library. It is moving its networking to longshore
(https://github.com/carverauto/longshore/issues/40).

The byte stream that is plain or TLS lives inside the HTTP client, where a
server cannot reach it, and any build of that crate compiles AWS-LC even
when every connection is plain.

## What Changes
- An accept that stays armed and reports each new connection
- A receive that stays armed and takes a buffer from a ring of buffers only
  when bytes arrive, so an idle connection holds none
- Completions say whether more will follow for their token, and which
  buffer they filled
- Socket options, no-delay and keep-alive, for the sockets a backend opens
  and accepts
- Unix-domain sockets: listen and connect by path
- `Stream`, plain or TLS over `Io`, moves from `longshore_http_client` to a
  crate of its own, `longshore_stream`
- TLS becomes a cargo feature, on by default. A build without it has no
  TLS code and no AWS-LC
- **BREAKING**: `Completion` gains fields; `Limits` gains the buffer ring;
  `longshore_http_client::stream` moves

## Impact
- Affected specs: `server-networking` (new)
- Affected code: `longshore_common` (`io`, ring backend, engine, both
  worlds), `longshore_stream` (new), `longshore_http_client`,
  `longshore_s3`, `longshore` (re-exports and features), `longshore-bench`
- Depends on: `add-portable-io-backend`
- Supersedes: the TLS-off feature in task 4.3 of `add-high-level-client`
- First principles: 2, the buffer ring and the socket range are sized at
  build. 3, a received frame stays in the buffer it arrived in until the
  caller gives the buffer back. 7, no dependency is added, and one is made
  optional
- Kernel: everything here is in Linux 6.1
