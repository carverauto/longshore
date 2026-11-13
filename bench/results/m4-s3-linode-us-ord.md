# Milestone 4 — S3 over plain HTTP against Akamai/Linode Object Storage

Run 2026-11-12 with `s3_uring` from host A (AArch64, Linux 6.14) to a bucket in `us-ord` (endpoint `us-ord-10.linodeobjects.com`, type E3), virtual-hosted style, port 80, no TLS.

## Correctness

`s3_uring selftest` with an 8 MiB object, heap allocation sealed for the whole exchange including DNS, signing and retries:

| Step | Outcome | Attempts |
|---|---|---|
| `store_no_clobber` on a new key | Stored | 1 |
| `store_no_clobber` on the same key | AlreadyExists | 1 |
| `fetch` | Fetched, 8,388,608 bytes, identical to what was stored | 1 |
| `fetch` range 1000-1999 | Fetched, 1,000 bytes, identical to that slice | 1 |
| `erase` | Erased | 1 |
| `fetch` after erase | NotFound | 1 |

One connection served all six requests.

## What this endpoint does (probed with curl before writing the client)

| Question | Observed |
|---|---|
| Plain HTTP on port 80 | Accepted |
| `x-amz-content-sha256: UNSIGNED-PAYLOAD` over plain HTTP | Accepted |
| Region in the signature scope | Not validated: a request signed for `us-east-1` succeeded |
| `If-None-Match: *` on PUT | Honoured: 412 on an existing key, 200 on a new one, existing object unchanged |
| `Range: bytes=a-b` | 206 with exactly those bytes |
| Wrong secret key | 403 with an XML error document |
| `x-amz-checksum-crc64nvme` with a deliberately wrong value | **Accepted with 200.** This endpoint does not enforce the checksum, so checksum rejection cannot be tested here |
| Rate limit | Response headers advertise 500 requests per second |
| Error responses | Carry an XML body even for PUT and DELETE; the HTTP client now reads and discards such bodies |

These are observations of one endpoint on one day, not statements about AWS S3.

## Throughput from inside the region

Client: a `g6-dedicated-8` Linode in `us-ord` (AMD EPYC 7713, 8 vCPU, Linux 7.0, x86-64, 6 Gbit/s outbound cap), one io_uring thread pinned to CPU 2, 8 MiB objects, plain HTTP, no checksums, 10 s per run, one run per cell. Every request succeeded on its first attempt. Raw output: `m4-s3-linode-us-ord-in-region.jsonl`.

| Request | Conns | MB/s | Thread CPU | GB per thread-CPU-second |
|---|---|---|---|---|
| PUT | 1 | 60 | 0.4% | 14.2 |
| PUT | 4 | 253 | 1.4% | 17.6 |
| PUT | 8 | 503 | 2.4% | 21.3 |
| PUT | 16 | 816 | 3.0% | 27.1 |
| PUT | 32 | 808 | 3.6% | 22.2 |
| GET | 1 | 157 | 6.0% | 2.6 |
| GET | 4 | 594 | 21.6% | 2.8 |
| GET | 8 | 1,129 | 38.5% | 2.9 |
| GET | 16 | 1,234 | 42.8% | 2.9 |
| GET | 32 | 1,229 | 43.8% | 2.8 |
| PUT, 1 MiB objects | 16 | 342 | 3.6% | 9.5 |

- **PUT tops out at about 816 MB/s (6.5 Gbit/s), which is the instance's outbound cap**, while using 3% of one core. Each connection carries about 60 MB/s, so the service limits a single stream and throughput comes from parallel connections.
- **GET tops out at about 1.23 GB/s (9.9 Gbit/s)** at 43% of one core. The client is not the limit here either.
- These numbers measure the instance's network allowance and the service, not the client's ceiling. They cannot be compared with the talk's 25.2 and 4.7 GB/s per core, which were network-unconstrained.
- GET costs about ten times more CPU per byte than PUT, the same asymmetry the talk attributes to the kernel's receive copy.

## Throughput from outside the region — not meaningful


| Request | Conns | MB/s | Thread CPU |
|---|---|---|---|
| PUT 8 MiB | 1 | 33.6 | 0.2% |
| PUT 8 MiB | 4 | 54.5 | 0.3% |
| PUT 8 MiB | 8 | 92.3 | 0.5% |
| GET 8 MiB | 4 | 106.9 | 3.5% |

Host A reaches Linode over its 1 GbE internet uplink, so these numbers measure that path. An end-to-end figure needs a client in the same region as the bucket.
