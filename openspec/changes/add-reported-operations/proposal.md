## Why
`store`, `fetch` and `stat` decide things for the caller. They retry up to
four times, a conditional store refused after a retry compares checksums to
guess whose object is there, and the result is a yes or no. A caller that
must account for every request cannot use them: an archive that deletes its
local copy only after the stored one is proven needs to send exactly one
request under a deadline, know whether it was sent, see what the store
answered, and read back the object the store named and no other.

## What Changes
- `put`, `head` and `get`, on the low-level store and on the shareable
  client, that end with a `Report`: not sent, answered with any status, or
  sent with no answer
- A limit per operation: how many requests it may send and a deadline on the
  monotonic clock. A deadline cancels a request in progress, and the result
  comes only when the kernel has let go of it
- `put` options: only if absent, SSE-S3 encryption, and a SHA-256 the caller
  supplies, sent in place of the CRC-64/NVME
- `head` and `get` bound to an identity: `If-Match` on an `ETag` and a
  version id
- The answer's own evidence: status, `ETag`, version id, request id, length,
  `Content-Range`, encryption and checksums, each absent when the store did
  not send it
- Evidence of the request that was sent: method, date, signed header names
  and the SHA-256 of the SigV4 canonical request, with nothing secret in it
- The HTTP client can cancel one request before its own timeout, and says
  whether a failed request's head was handed to the kernel

## Impact
- Affected specs: `reported-operations` (new)
- Affected code: `longshore_s3::report` (new), `longshore_s3::s3` (three
  operations, deadlines), `longshore_s3::client` (three operations,
  `Budget`), `longshore_s3::sigv4` (the canonical request hash on its own),
  `longshore_http_client::client` (`cancel`, `Done::sent`), the test server
  (object identity, preconditions, evidence headers)
- First principles: nothing allocates per operation; a report is `Copy` and
  inline. Bodies are still never hashed with SHA-256 by the client. No lock
  or runtime is added to the data path
- Not included: multipart completion that is conditional or reconciled after
  a lost answer, an HTTP CONNECT proxy, Google Cloud Storage for these three
  operations
