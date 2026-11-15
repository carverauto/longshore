# Milestone 24: `put`, `head` and `get` against Linode Object Storage

Run 2026-11-14 with `exact_s3` at commit `0ff93e8` from host A (DGX Spark,
AArch64, Linux 6.14) over the internet to Linode Object Storage, endpoint
`us-ord-10.linodeobjects.com`, signing region `us-ord`, over HTTPS, with the
bucket in the host name. One run. The bucket keeps no versions. The key
used was limited to that bucket. Every line the program printed is in
`m24-put-head-get-linode.txt`.

Each operation was one request (`Budget::once`), 300,000 bytes per object.

## What the store did

| Asked | Answer |
|---|---|
| `put`, only if absent, encryption asked for, SHA-256 | 200, `ETag`, encryption `AES256`. No SHA-256 in the answer |
| `put`, only if absent, SHA-256, no encryption asked for | 200, `ETag`, encryption `AES256` |
| `put`, only if absent, encryption asked for, CRC-64 | 200, `ETag`, encryption `AES256`. No CRC-64 in the answer |
| `put`, only if absent, CRC-64 | 200, `ETag`, encryption `AES256` |
| The first `put` again | 412 |
| `head` with the returned `ETag` | 200, length 300000, same `ETag`, encryption `AES256`. No checksum of either kind |
| `head` with another `ETag` | 412 |
| `get` with the returned `ETag` | 200, 300,000 bytes, equal to the bytes written |
| `get` of bytes 1000 to 1999 with the returned `ETag` | 206, `Content-Range: bytes 1000-1999/300000`, equal to those bytes |
| `get` with another `ETag` | 412, and a 267-byte error document as the body |
| `put` with the SHA-256 of other bytes | **200. The object was stored** |
| `put` after its deadline | Not sent |

Every answer carried a request id. No answer carried a version id.

## Reading it

- **`If-None-Match: *` and `If-Match` work.** A second write to a key is
  refused, and a read that names another `ETag` is refused, on `head` and on
  `get`.
- **The store says every object is encrypted, asked or not.** All four
  writes came back with `x-amz-server-side-encryption: AES256`, and so did
  `head` and `get`. A caller that requires that header in the answer gets
  it. It is not evidence that the request for encryption had any effect.
- **The store does not check or keep the SHA-256.** A write whose
  `x-amz-checksum-sha256` was the hash of other bytes was accepted with
  200, and no answer returned a SHA-256. The same holds for the CRC-64: none
  came back from `put`, `head` or `get`, so a whole-object `get` here is
  not verified by this client (`verified` is `None`). On this store the
  checksum sent with a write protects nothing. What protects the body in
  transit is TLS, and what proves the stored bytes is reading them back and
  hashing them, which the caller has to do.
- **The `ETag` is 32 hex digits and changed with the body**, as an MD5 of
  the content would. That was not checked against an MD5 computed here.
- **Ranges are served exactly**, with the object's length in `Content-Range`.
- **A refused `get` writes the error document into the caller's buffer.**
  `body_len` was 267 with status 412. The bytes in the buffer are the
  object's only when the status is 200 or 206.

## What this does not tell us

- Anything about Amazon S3, MinIO or Ceph. Amazon S3 is expected to refuse
  the wrong SHA-256 and to return checksums; that has not been run.
- A bucket with versioning on: no version id was returned or sent.
- A deadline that cancels a request in flight against a real store, a lost
  answer, or more than one attempt. Those are covered in the simulator and,
  for the cancel, on real rings against an address that does not answer.
- x86-64 against this store. The run was on AArch64 only.
- Other Linode regions or endpoint generations, which may differ.
- Repeatability: one run.
