## 1. HTTP client
- [x] 1.1 `Done::sent`: whether a request's head was handed to the `Io`
- [x] 1.2 `Client::cancel`: stop one request before its own timeout

## 2. Store
- [x] 2.1 `report` types: `Put`, `Expect`, `Limit`, `Report`, `End`, `Answer`, `Signed`, all `Copy` and inline
- [x] 2.2 `Checksum::Sha256`, sent in place of the CRC-64/NVME
- [x] 2.3 `put`, `head` and `get` with `If-None-Match`, `If-Match`, `versionId`, SSE-S3 and ranges, signed
- [x] 2.4 Attempts and deadline per operation; a deadline cancels a request in progress
- [x] 2.5 The canonical request hash and signed header names as evidence
- [x] 2.6 Simulation tests: identity-bound readback, refusal, wrong identity, short range, absent evidence, one attempt, lost answers, deadlines

## 3. Shareable client
- [x] 3.1 `Client::put`, `Client::head`, `Client::get` and `Budget`
- [x] 3.2 An I/O thread that fails reports a sent request as unknown, not as closed
- [x] 3.3 Tests over driven workers

## 4. Real kernels and stores
- [x] 4.1 `client_uring` covers the three operations and a deadline that cancels a connect
- [x] 4.2 Example program `verified_write`
- [x] 4.3 README: status row, how to use them, limits
- [ ] 4.4 Run against Amazon S3 and Linode Object Storage, recorded in `bench/results/`
