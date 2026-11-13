## 1. Metadata
- [x] 1.1 `stat` with a fixed-size result; simulation tests including a missing key
- [x] 1.2 `Options` value (content type, user metadata, storage class, encryption headers) with fixed capacity and signing of the added headers
- [x] 1.3 Options accepted by store, conditional store and multipart begin

## 2. Server-side operations
- [x] 2.1 `copy`, including the error document returned with status 200
- [x] 2.2 `erase_many` with a manifest helper and a per-key result parser
- [x] 2.3 Delimiter listing and common prefixes; property tests and fuzz corpus update
- [x] 2.4 Continuation tokens for listing, so a truncated delimiter listing can be resumed (`S3::list_continue`; run against Amazon S3 and Linode Object Storage)

## 3. Presigned URLs
- [x] 3.1 Query-string SigV4 for fetch and store, written into a caller buffer, checked against published examples
- [x] 3.2 Refuse expiry beyond the store's limit
- [ ] 3.3 Refuse expiry beyond the life of temporary credentials. Waits for `add-credential-providers`, which gives credentials an expiry time

## 4. Real stores
- [x] 4.1 `s3_uring objects` covers every new operation
- [x] 4.2 Runs against Amazon S3, Linode Object Storage and Google Cloud Storage, recorded in `bench/results/`
- [x] 4.3 README: operation table
