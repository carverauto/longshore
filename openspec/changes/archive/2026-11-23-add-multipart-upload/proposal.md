## Why
A store operation sends one registered buffer, so an object can be no larger
than the buffer size the client was built with. Multipart upload removes that
limit and is the only way to upload when the total length is not known at the
start.

## What Changes
- Four store operations: begin a multipart upload, store a part, complete,
  and abort
- Fixed-size types for the upload id and part ETags, and a helper that writes
  the completion document into a caller buffer
- CRC-64/NVME per part, and the whole-object checksum on completion where the
  store accepts one
- Completion treats a 200 response with an error document as a failure
- The simulator's test server gains multipart so every path is covered by
  simulation tests

## Impact
- Affected specs: `multipart-upload` (new)
- Affected code: `store::s3` (new operation kinds, a query field on the
  operation slot), `store::sigv4` (query parameters for part number and upload
  id), `http::server` (test server), `longshore-bench` (`s3_uring` commands)
- First principles: each part is one registered buffer sent with `SEND_ZC`;
  no allocation is added
