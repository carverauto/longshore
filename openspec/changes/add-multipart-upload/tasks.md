## 1. Types and encoding
- [ ] 1.1 `UploadId` and `PartTag` fixed-size types with tests for the length limits
- [ ] 1.2 Parser for the begin response and for the completion response, including the error document returned with status 200; property tests and a fuzz target
- [ ] 1.3 `multipart::write_manifest` with tests for ordering and buffer overflow
- [ ] 1.4 SigV4 canonical query for `uploads`, `partNumber` and `uploadId`, checked against published examples

## 2. Operations
- [ ] 2.1 Begin, store part, complete and abort in `store::s3`, with a query field on the operation slot
- [ ] 2.2 Per-part checksum and whole-object checksum on completion
- [ ] 2.3 Retry rules for each operation, including the completion comparison
- [ ] 2.4 Google Cloud Storage request forms

## 3. Tests
- [ ] 3.1 Multipart in the simulator's test server
- [ ] 3.2 Simulation tests: round trip, parts out of order and in parallel, a fault in every phase, abort, completion answered with an error document
- [ ] 3.3 Allocation guard stays sealed through all of the above

## 4. Real stores
- [ ] 4.1 `s3_uring` commands for a multipart round trip and throughput
- [ ] 4.2 Runs against Amazon S3, Linode Object Storage and Google Cloud Storage, recorded in `bench/results/`
- [ ] 4.3 README: operation list and limits
