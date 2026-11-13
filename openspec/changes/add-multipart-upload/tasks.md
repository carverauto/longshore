## 1. Types and encoding
- [x] 1.1 `UploadId` and `PartTag` fixed-size types with tests for the length limits
- [x] 1.2 Parser for the begin response and for the completion response, including the error document returned with status 200; property tests and a fuzz target
- [x] 1.3 `multipart::write_manifest` with tests for ordering and buffer overflow
- [x] 1.4 SigV4 canonical query for `uploads`, `partNumber` and `uploadId`, exercised by the documented presigned URL example and accepted by the real stores

## 2. Operations
- [x] 2.1 Begin, store part, complete and abort in `store::s3`, with a query field on the operation slot
- [x] 2.2 Per-part checksum, repeated per part in the completion document, and whole-object checksum on completion
- [x] 2.3 Retry rules for each operation, including the completion comparison
- [x] 2.4 Google Cloud Storage request forms

## 3. Tests
- [x] 3.1 Multipart in the simulator's test server
- [x] 3.2 Simulation tests: round trip, parts out of order and in parallel, a fault in every phase, abort, completion answered with an error document
- [x] 3.3 Allocation guard stays sealed through all of the above

## 4. Real stores
- [x] 4.1 `s3_uring` commands for a multipart round trip and throughput
- [x] 4.2 Runs against Amazon S3, Linode Object Storage and Google Cloud Storage, recorded in `bench/results/`
- [x] 4.3 README: operation list and limits
