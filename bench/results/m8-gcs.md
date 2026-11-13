# Milestone 8 — Google Cloud Storage, real bucket

Run 2026-11-12 with `s3_uring` (`S3_VENDOR=gcs`) from host A (DGX Spark, AArch64, Linux 6.14) over the internet to a bucket in `us-central1`, HTTPS, one io_uring thread. Authentication: a short-lived bearer token for a service account that can manage objects in that bucket only. The organisation forbids service-account keys, including HMAC keys, so the SigV4 route was not available.

## What was verified

| Check | Result |
|---|---|
| Selftest, 8 MiB and 100 kB objects: conditional store, the same again, fetch, ranged fetch, erase, fetch after erase | All as expected, every byte compared, allocation sealed outside the handshake |
| Conditional store onto an existing object (`x-goog-if-generation-match: 0`) | 412, reported as already existing |
| Whole-object fetch | The store returns `x-goog-hash: crc32c=...`; the client checks the bytes against it (`verified: true`). This is the first store tried that returns a checksum on fetch |
| Store with a deliberately wrong CRC-32C | **Refused with 400 `CrcMismatch`**, every attempt, and nothing is stored. The client treats it as damage in transit and retries, then reports `RetriesExhausted` |
| Listing with a prefix (ListObjectsV2 form) and erase by listing | Work unchanged from S3 |

## Corrections to what the documentation suggested

- The error code for a checksum mismatch is `CrcMismatch`, not `BadDigest` (which the reference documents for `Content-MD5`). The client first reported it as a fatal 400; it now retries.
- The store sends two `x-goog-hash` headers, CRC-32C first and MD5 second. The client reads the first header with that name and looks for `crc32c=` in it; if a response ever put MD5 first, the fetch would be reported unverified (or retried when checksums are required), never accepted wrongly.

## Throughput

Not measured meaningfully: 63 MB/s PUT and 105 MB/s GET with 8 connections is the site's internet link, not the client (the client thread used 3% and 14% of a core). An in-region run would be needed for a real figure.

## Caveats

- One run of each check.
- The token was minted by impersonation with `gcloud`; reading a token file and rotating it is covered in the simulator only.
- S3's own mismatch code is still assumed to be `BadDigest`: Linode does not enforce checksums and no enforcing S3 store has been tried.
