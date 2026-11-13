## 1. Metadata
- [ ] 1.1 `stat` with a fixed-size result; simulation tests including a missing key
- [ ] 1.2 `Options` value (content type, user metadata, storage class, encryption headers) with fixed capacity and signing of the added headers
- [ ] 1.3 Options accepted by store, conditional store and multipart begin

## 2. Server-side operations
- [ ] 2.1 `copy`, including the error document returned with status 200
- [ ] 2.2 `erase_many` with a manifest helper and a per-key result parser
- [ ] 2.3 Delimiter listing and common prefixes; property tests and fuzz corpus update

## 3. Presigned URLs
- [ ] 3.1 Query-string SigV4 for fetch and store, written into a caller buffer, checked against published examples
- [ ] 3.2 Refuse expiry beyond the store's limit and beyond the life of temporary credentials

## 4. Real stores
- [ ] 4.1 `s3_uring selftest` covers every new operation
- [ ] 4.2 Runs against Amazon S3, Linode Object Storage and Google Cloud Storage, recorded in `bench/results/`
- [ ] 4.3 README: operation table
