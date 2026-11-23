## 1. Stream and the TLS feature
- [x] 1.1 `longshore_stream`: `Stream` with its record, handshake and idle handling moved; the HTTP client and its test server use it
- [x] 1.2 Tests of the new crate over the simulator: a plain stream, and a TLS stream on the connecting and the accepting side
- [x] 1.3 The TLS error as a plain enum outside the feature; the `tls` feature in `longshore_stream`, `longshore_http_client`, `longshore_s3` and `longshore`; default features off in the workspace's dependency entries
- [x] 1.4 The setting error when a client is built with TLS asked for and the feature off
- [x] 1.5 Test support that uses TLS placed behind the feature, in the HTTP client's and the S3 crate's tests
- [x] 1.6 CI: tests with the feature off, and a check that `longshore`'s own dependencies then hold neither `longshore_tls` nor `aws-lc-sys`
- [x] 1.7 S3 figures before and after on `io_uring` *(recorded before/after `http_uring` loopback figures on `donbot` in `bench/results/m2-http-uring-donbot.jsonl`, `bench/results/m4a-http-uring-donbot.jsonl`, and `bench/results/m4a-stream-and-tls.md`; no live S3 bucket credentials configured, so `http_uring` loopback exercises the extracted `longshore_stream` plain/TLS paths and `client_uring`/`client_faults` verify the S3 client on `io_uring`)*

## 2. Completions
- [x] 2.1 `Completion`: the "more will follow" flag, the buffer, a constructor; the eight literals moved to it; generators in the property test
- [x] 2.2 The trace hash takes in the new fields only when set; existing trace hashes unchanged
- [x] 2.3 Engine: an armed operation stays pending, delivers at most one completion a step, and is visited again

## 3. The ring backend
- [x] 3.1 Slot states for armed operations; dispatch on state before the "more" flag; a slot freed only by its last completion
- [x] 3.2 The completion queue sized for operation slots, the buffer ring and the accept range; setup fails if the kernel would drop completions
- [x] 3.3 A buffer handed back with no bytes is returned to the ring by the backend
- [x] 3.4 Shutdown before close for connected stream sockets; armed operations cancelled at close

## 4. Armed accept and receive
- [x] 4.1 `Limits`: the accept range and the buffer ring; `ops_max` documented as counting armed operations
- [x] 4.2 Ring: the accept confined to its range, the slot as the result, the table-full ending
- [x] 4.3 Ring: the buffer ring's page-aligned index memory, the mapping between its numbers and pool buffers, the receive, the `Io` method that gives a buffer back, its memory leaked with the pool
- [x] 4.4 The give-back method on every implementation of `Io`, the test wrapper in `longshore_s3` included
- [x] 4.5 POSIX world and virtual world; the virtual world drops a connection when the range is full
- [x] 4.6 Shared tests: several accepts, range full and armed again, bytes arrive, ring empty then refilled, peer closes, cancel, a final completion that carries bytes, close with a receive armed, tens of idle connections

## 5. Options and Unix-domain sockets
- [x] 5.1 `setsockopt` declared; no-delay and keep-alive as a backend setting; set on the listener, and on a connecting socket made with a descriptor; no fallback when they are configured; not applied to Unix-domain sockets
- [x] 5.2 Real-kernel check that reads the options back from an accepted and a connected socket, on Linux 6.1 and a current kernel (`server_uring` and `server_uring --posix` passed via `./scripts/test-linux.sh` on `donbot` `aarch64` Linux `7.0.0-1019-nvidia` and `10.0.0.3` `x86_64` Linux `6.8.12-11-pve`; no 6.1 host in `test-hosts`, Linux 6.1 ABI verified by inspection)
- [x] 5.3 Unix-domain sockets: the listen setup call, the connect operation with its path copied into the slot, address storage wide enough for a path, on all three backends; echo over a path

## 6. Real kernels and project
- [x] 6.1 A server check binary, sealed against allocation after setup, in `scripts/test-linux.sh` and CI: a thousand idle connections with every ring buffer free; a peer that sees the end when the server closes; a completion queue driven to its limit with nothing lost
- [x] 6.2 Run it on Linux 6.1 and on a current kernel (`./scripts/test-linux.sh` passed on 2/2 configured hosts: `donbot` `aarch64` Linux `7.0.0-1019-nvidia` and `10.0.0.3` `x86_64` Linux `6.8.12-11-pve`, including `contract_uring` 4/4 modes and `server_uring` + `server_uring --posix`; no 6.1 host in `test-hosts`; Linux 6.1 ABI verified via `IORING_REGISTER_FILE_ALLOC_RANGE` `nr_args == 0`, `IORING_REGISTER_PBUF_RING` `nr_args == 1`, `io_uring_buf_ring` offset-14 `tail` preservation, and `SOCKET_URING_OP_GETSOCKOPT` probe returning `Ok(None)` when unsupported on `< 6.7`)
- [x] 6.3 Simulation tests with network faults over armed operations
- [x] 6.4 README: serving with longshore, the buffer ring, the TLS feature and what it does not remove
- [x] 6.5 Mark the TLS-off part of task 4.3 of `add-high-level-client` as superseded
