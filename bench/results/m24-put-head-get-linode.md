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
| `put` with the SHA-256 of other bytes | **200. The object was stored** (its body was not the test pattern: see below) |
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
- **In this run the body of that write was not the test pattern.** At
  commit `0ff93e8` the program reused the buffer the preceding `get`s had
  written into, so the 300,000 bytes sent began with the 267-byte error
  document of the refused `get` and the range bytes left before it. Its
  `ETag` differs from the other four objects' for that reason. The
  conclusion stands: the store accepted a body whose
  `x-amz-checksum-sha256` was the hash of other bytes. The program now
  restores the pattern before that write and before the late one.
- **The `ETag` is 32 hex digits**, and the four writes of the same bytes
  got the same one, as an MD5 of the content would. That was not checked
  against an MD5 computed here.
- **Ranges are served exactly**, with the object's length in `Content-Range`.
- **A refused `get` writes the error document into the caller's buffer.**
  `body_len` was 267 with status 412. The bytes in the buffer are the
  object's only when the status is 200 or 206.

## Run again after the fix

After the fix the program was run again at commit `81238a9`, once from
host A and once from host B (x86-64, Linux 6.8), over HTTPS to a second
bucket in the same Linode region: a longshore test bucket, not the one
used by the first run. Both ended `ok exact_s3` with exit status 0. Every
line each printed is in `m24-put-head-get-linode-fixed-aarch64.txt` and
`m24-put-head-get-linode-fixed-x86_64.txt`.

The store gave the same answers as in the first run, with two differences:

- **The write with the SHA-256 of other bytes now sends the test
  pattern**, and its `ETag` is the same as the other four objects'. That
  shows directly that the store kept the pattern although the
  `x-amz-checksum-sha256` header was the hash of other bytes.
- **The error document of a refused request was 262 bytes** there, not
  267, on the refused `put`, `head` and `get`.

## What this does not tell us

- Anything about Amazon S3, MinIO or Ceph. Amazon S3 is expected to refuse
  the wrong SHA-256 and to return checksums; that has not been run.
- A bucket with versioning on: no version id was returned or sent.
- A deadline that cancels a request in flight against a real store, a lost
  answer, or more than one attempt. Those are covered in the simulator and,
  for the cancel, on real rings against an address that does not answer.
- Other Linode regions or endpoint generations, which may differ.
- Repeatability beyond three runs over two buckets, two from AArch64 and
  one from x86-64.
