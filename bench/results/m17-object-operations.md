# Milestone 17: object operations against real stores

Run 2026-11-13 with `s3_uring objects` from one Arm host (DGX Spark,
Linux 6.14) over the public internet. A correctness run; the timings say
nothing about throughput. Heap allocation is sealed for the whole check.

The check stores an 8 MiB object with a content type and one metadata
entry, reads its metadata with `stat`, copies it on the server, reads the
copy's metadata, lists the prefix with a delimiter, erases both keys, and
confirms with `stat` that they are gone. It then writes a presigned URL.

| Step | Amazon S3 (HTTPS) | Linode Object Storage (HTTP) | Google Cloud Storage (HTTPS) |
|---|---|---|---|
| Store with options | Passed | Passed | Passed |
| `stat` | Passed, with a CRC-64/NVME | Passed, no checksum returned | Passed, with a CRC-32C |
| Server-side copy | Passed | Passed | Passed |
| `stat` of the copy | Passed | Passed | Passed |
| Listing with a delimiter | Passed | Passed | Passed |
| Batch erase | Passed | Passed | Not offered; two single erases |
| `stat` after erase | Not found | Not found | Not found |
| Presigned URL | Accepted | Accepted | Not possible with a bearer token |

Every request succeeded on its first attempt, and a listing of the prefix
afterwards was empty on all three stores.

What this settles:

- Linode Object Storage accepts a batch erase that carries a CRC-64/NVME
  header and no `Content-MD5`. The client has no MD5, so a store that insists
  on one would refuse the request. That was the open risk for S3-compatible
  stores, and it did not arise here.
- Linode does not return a checksum on `stat` for an object stored without
  one. Amazon S3 and Google Cloud Storage return theirs.
- Presigned URLs were checked by fetching them with `curl`, with no other
  credentials. For a key that had just been erased, both Amazon S3 (signed
  with temporary credentials and their session token) and Linode answered
  404, which means the signature was accepted. With the last character of
  the signature changed, both answered 403.

Not covered here: a copy answered with status 200 and an error document, a
lost copy response, a batch erase in which the store refuses some keys, and
options resent on a retry. Those are exercised in the simulator
(`crates/longshore/tests/store/s3_object_ops.rs`).
