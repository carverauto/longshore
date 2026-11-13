# Milestone 16: multipart upload against real stores

Run 2026-11-13 with `s3_uring multipart` from one Arm host (DGX Spark,
Linux 6.14) over the public internet. This is a correctness run. The host is
not in any store's region, so the timings say nothing about throughput.

The check uploads one 24 MiB object as three 8 MiB parts, each from its own
registered buffer on its own connection, at the same time. It then completes
the upload, reads every part back by range and compares the bytes, erases
the object, and finally begins a second upload, stores one part and aborts
it. Heap allocation is sealed for all of it.

| Store | Transport | Part checksums | Whole-object checksum on completion | Result |
|---|---|---|---|---|
| Amazon S3, us-east-1 | HTTPS | CRC-64/NVME | CRC-64/NVME | Passed |
| Amazon S3, us-east-1 | HTTPS | None | None | Passed |
| Linode Object Storage, us-ord | HTTP | CRC-64/NVME | CRC-64/NVME | Passed |
| Linode Object Storage, us-ord | HTTP | None | None | Passed |
| Google Cloud Storage | HTTPS | CRC-32C | Not sent | Passed |

In every run each part was stored on its first attempt, the completion
succeeded on its first attempt, the bytes read back matched, and a listing
of the prefix afterwards was empty.

What this settles:

- Amazon S3 accepts a CRC-64/NVME on each part and the whole-object value on
  completion when the upload is begun with `x-amz-checksum-algorithm:
  CRC64NVME` and `x-amz-checksum-type: FULL_OBJECT`.
- Linode Object Storage accepts the same headers.
- Google Cloud Storage's XML API accepts a CRC-32C in `x-goog-hash` on each
  part. The client does not send a whole-object checksum there, because the
  one it has is CRC-64.

Not covered here: a completion whose response is lost, a store answering a
completion with status 200 and an error document, and damaged parts. Those
are exercised in the simulator (`crates/longshore/tests/store/s3_multipart.rs`),
since a real store cannot be made to do them on demand.
