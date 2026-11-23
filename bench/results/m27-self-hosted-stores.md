# Milestone 27: self-hosted stores with the bucket in the path, Ceph and RustFS

Run 2026-11-23 with `scripts/self-hosted-s3.sh` at commit `b078ce1`, on an
Apple M3 Max under OrbStack (Docker 29.8.2, Linux
`7.2.12-orbstack` on AArch64). Each store ran in its own container, and the
programs ran in a `rust:1-bookworm` container on the same Docker network,
built there, with the seccomp profile off and locked memory unlimited. Plain
HTTP on port 80. The host name of each store is its container name,
resolved by Docker's DNS through `/etc/resolv.conf` like any other name. One
run per store. Every line the programs printed is in
`m27-self-hosted-ceph.txt` and `m27-self-hosted-rustfs.txt`.

- **Ceph 20.2.5** (`quay.io/ceph/ceph:v20.2.5`): one monitor, one manager,
  one BlueStore OSD on an 8 GiB file, and the S3 gateway (RGW, `beast`
  frontend), with cephx. Pools have one replica.
- **RustFS 1.0.6** (`rustfs/rustfs:1.0.6`): one node, one volume.

Neither store was given a DNS name (`rgw_dns_name` on Ceph,
`RUSTFS_SERVER_DOMAINS` on RustFS), so each serves only requests with the
bucket in the path. Every program ran with `S3_PATH_STYLE=1`. `exact_s3`
goes through the shareable client (`Builder::bucket_in_path`). The
`s3_uring` commands drive the store directly.

## Result

| Program | Ceph 20.2.5 | RustFS 1.0.6 |
|---|---|---|
| `s3_uring selftest`: conditional store, the same again, fetch, range, erase, fetch after erase | ok, allocation sealed | ok, allocation sealed |
| `s3_uring objects`: options, stat, copy, delimiter listing paged by token, batch erase, presigned URL | ok | ok |
| `s3_uring multipart`: three 8 MiB parts with checksums, complete, abort | ok | ok |
| `s3_uring badcrc`: store with a wrong CRC-64/NVME | **stored**, `checksum_enforced=false` | **refused** every attempt, then `RetriesExhausted`; nothing stored |
| `exact_s3` | ok | ok |

Every operation finished on its first attempt except the refused ones.

## What each store answered to `exact_s3`

| Asked | Ceph 20.2.5 | RustFS 1.0.6 |
|---|---|---|
| `put`, only if absent, encryption (SSE-S3) asked for | 400 | 400 |
| `put`, only if absent, SHA-256 | 200, no checksum in the answer | 200, SHA-256 returned and matches |
| `put`, only if absent, CRC-64 | 200, no checksum in the answer | 200, CRC-64 returned |
| The first `put` again | 412 | 412 |
| `head` and `get` with the returned `ETag` | 200, no checksum | 200, the SHA-256 returned |
| `get` of bytes 1000 to 1999 | 206, exact `Content-Range` | 206, exact `Content-Range` |
| `head` and `get` with another `ETag` | 412 | 412 |
| `put` with the SHA-256 of other bytes | **200. Stored** | **400. Refused** |
| `put` after its deadline | Not sent | Not sent |

Bytes read back, whole and ranged, equalled the bytes written on both.

## Reading it

- **Path addressing works through both the store and the client** against
  stores that accept nothing else.
- **Ceph's gateway checks no checksum here.** It stored a body whose
  CRC-64/NVME was wrong and one whose SHA-256 was that of other bytes, and
  returned no checksum on `put`, `head` or `get`, so a whole-object fetch
  is not verified by the client (`verified: false`). This is the same as
  Linode in `m24-put-head-get-linode.md`. Only the gateway's defaults were
  tried.
- **RustFS checks both and returns them.** A whole-object fetch is
  verified against the checksum it returns (`verified: true`). It is the
  first S3-API store tried here that enforces CRC-64/NVME.
- **RustFS names a checksum mismatch `BadDigest`**, the code the client
  assumes for S3 (`crates/longshore_s3/src/s3/outcome.rs`). Sent by hand
  with a wrong `x-amz-checksum-crc64nvme`, the answer was 400 with
  `<Code>BadDigest</Code>` (and a message about `Content-Md5`). The client
  read the refusal as damage in transit, retried, and reported
  `RetriesExhausted`, as it does on Google Cloud Storage. Whether Amazon S3
  uses the same code is still not observed.
- **SSE-S3 is refused by both** with 400. Neither had a key service
  configured, which both need for SSE-S3. The refusal is reported, not
  hidden: the caller sees the status.

## What this does not tell us

- Anything about Amazon S3, or about MinIO.
- Speed: both stores ran on one laptop with one disk image, so no throughput
  was measured.
- TLS, IPv6, or a store across a real network. The containers share a
  virtual bridge.
- Ceph with more than one OSD, erasure coding, or a gateway configured with
  a DNS name, encryption or checksum settings other than its defaults.
- Repeatability beyond one run per store.
