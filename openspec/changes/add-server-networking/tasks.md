## 1. Stream and the TLS feature
- [ ] 1.1 `longshore_stream`: `Stream` and its record handling moved, tests moved with it
- [ ] 1.2 `tls` feature in `longshore_stream`, `longshore_http_client`, `longshore_s3` and `longshore`; the error when TLS is asked for without it
- [ ] 1.3 CI builds and tests with the feature off, and checks the dependency tree
- [ ] 1.4 A server-side stream test: accept, handshake, read plaintext
- [ ] 1.5 S3 figures before and after on `io_uring`

## 2. Completions
- [ ] 2.1 `Completion`: the "more will follow" flag and the buffer number; every existing operation leaves them clear
- [ ] 2.2 Ring backend: a slot lives until its final completion
- [ ] 2.3 Engine: an armed operation stays pending and is attempted again

## 3. Armed accept and receive
- [ ] 3.1 `Limits`: the slot range for accepted sockets and the buffer ring
- [ ] 3.2 Ring: accept into a slot the kernel allocates; receive from a registered buffer ring; giving a buffer back
- [ ] 3.3 POSIX world and virtual world, from the engine's rule
- [ ] 3.4 Shared tests: several accepts, range full, bytes arrive, ring empty, peer closes, cancel, a thousand idle connections

## 4. Options and Unix-domain sockets
- [ ] 4.1 No-delay and keep-alive as a backend setting; listener and connect routes
- [ ] 4.2 Real-kernel check that reads the options back, on Linux 6.1 and a current kernel
- [ ] 4.3 Unix-domain listen and connect on all three backends; echo over a path

## 5. Real kernels and project
- [ ] 5.1 A server check binary, sealed against allocation after setup, in `scripts/test-linux.sh` and CI
- [ ] 5.2 Simulation tests with network faults over armed operations
- [ ] 5.3 README: serving with longshore, the TLS feature
- [ ] 5.4 Mark the TLS-off part of task 4.3 of `add-high-level-client` as superseded
