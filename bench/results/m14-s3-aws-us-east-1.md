# Milestone 14 — Real Amazon S3, in region

Measured 2026-11-13 with `s3_uring` on one `c8gn.8xlarge` instance (Graviton4, 32 vCPUs, 100 Gbit/s network, Linux 7.0) in us-east-1, against a general-purpose S3 bucket in the same region. One client io_uring thread pinned to CPU 2, 8 MiB objects, 8 s per run, one run per cell. GB = 10^9 bytes. Raw output: `m14-s3-aws-us-east-1.jsonl`.

Unlike the earlier throughput runs, the peer here is a real object store. The client used the instance role's temporary keys, with their session token. It resolves the bucket's endpoint itself and rotates through the addresses in each answer as it opens connections; the endpoint was returning eight addresses at the time. How the connections were divided among them was not recorded.

## Correctness

- The self-test passes over HTTP and HTTPS: conditional store, the same again (refused as already existing), fetch, ranged fetch, erase, fetch after erase, every byte compared, allocation sealed outside the handshake.
- **S3 returns a CRC-64/NVME on fetch when asked**, and the client verifies the bytes against it (`verified: true`).
- **S3 enforces the checksum on store.** A body sent with a deliberately wrong checksum was refused on every attempt and never stored. The client retried and reported `RetriesExhausted`, which it only does when the error code is `BadDigest`: the assumption the client was written on is now confirmed.

## Throughput, plain HTTP

| Request | Conns | GB/s | Gbit/s | Client thread |
|---|---|---|---|---|
| PUT | 8 | 0.63 | 5 | 0.04 |
| PUT | 32 | 2.60 | 21 | 0.12 |
| PUT | 64 | 5.01 | 40 | 0.27 |
| PUT | 128 | 10.17 | 81 | 0.47 |
| PUT | 192 | **12.28** | 98 | 0.16 |
| PUT | 256 | 12.21 | 98 | 0.15 |
| PUT with CRC-64/NVME | 8 | 0.65 | 5 | 0.05 |
| PUT with CRC-64/NVME | 32 | 2.57 | 21 | 0.25 |
| PUT with CRC-64/NVME | 64 | 5.13 | 41 | 0.43 |
| PUT with CRC-64/NVME | 128 | 10.18 | 81 | 0.74 |
| PUT with CRC-64/NVME | 192 | **12.24** | 98 | 0.67 |
| PUT with CRC-64/NVME | 256 | 12.17 | 97 | 0.66 |
| GET | 8 | 0.77 | 6 | 0.23 |
| GET | 32 | 3.07 | 25 | 0.79 |
| GET | 64 | 4.01 | 32 | 1.00 |

Every request in every run succeeded on its first attempt.

## Throughput, HTTPS

| Request | Conns | GB/s | Client thread |
|---|---|---|---|
| PUT | 8 | 0.53 | 0.19 |
| PUT | 32 | 2.13 | 0.62 |
| PUT | 64 | 2.89 | 0.95 |
| PUT with CRC-64/NVME | 64 | 2.52 | 0.77 |
| GET | 32 | 2.54 | 0.96 |
| GET | 64 | 2.52 | 0.99 |

## Request rates, HTTPS, 32 connections

| Operation | Requests/s | Median below | 99% below | Client CPU per request |
|---|---|---|---|---|
| Conditional store, new 4 KiB key | 473 | 131 ms | 131 ms | 36 µs |
| Ranged fetch, 4 KiB of 1 MiB | 1,283 | 33 ms | 66 ms | 28 µs |
| Erase | 1,667 | 33 ms | 66 ms | 24 µs |
| List 200 keys | 1,185 | 33 ms | 66 ms | 78 µs |

## Repeated runs, with the whole machine's CPU

The table above is one run per cell and reports only the client thread's own CPU time. That figure is not a measure of what the upload costs: with many connections the kernel does much of the network work in interrupt context on other cores, where the thread's clock does not see it. The single runs showed the thread 47% busy at 128 connections and 16% at 192, which cannot both describe the cost of the work.

So the uploads were run again on a second instance of the same type, five times per cell, also recording how busy the whole 32-core machine was (from `/proc/stat`, in cores). Raw output: `m14-s3-aws-us-east-1-repeated.jsonl`. Median, with the lowest and highest in brackets:

| Request | Conns | GB/s | Client thread | Whole machine, cores busy |
|---|---|---|---|---|
| PUT | 64 | 5.13 (5.03 to 5.18) | 0.25 (0.20 to 0.31) | 1.55 (1.37 to 1.68) |
| PUT | 128 | 10.24 (10.12 to 10.36) | 0.43 (0.35 to 0.53) | 2.44 (1.97 to 2.91) |
| PUT | 192 | 12.15 (12.10 to 12.23) | 0.18 (0.17 to 0.19) | 3.02 (2.30 to 3.44) |
| PUT with CRC-64/NVME | 64 | 5.11 (4.90 to 5.15) | 0.41 (0.36 to 0.50) | 1.51 (1.35 to 1.58) |
| PUT with CRC-64/NVME | 128 | 10.01 (9.79 to 10.10) | 0.79 (0.77 to 0.89) | 2.45 (2.42 to 3.23) |
| PUT with CRC-64/NVME | 192 | 12.19 (12.08 to 12.23) | 0.60 (0.58 to 0.60) | 2.59 (2.23 to 3.36) |

One request out of 260,943 needed a second attempt (a plain PUT at 128 connections).

- **The throughput figures hold**: from slowest to fastest of the five runs, a cell spreads by at most 1.3% at 192 connections, 3.1% at 128 and 5.1% at 64, and each median is within 2.5% of the single run.
- **Filling the 100 Gbit/s card costs the machine about three cores**, of which the client's thread is a fraction. The rest is the kernel handling 192 connections' worth of acknowledgements and transmit completions on whichever cores the card's interrupts land.
- **The thread's own share is not a stable number** and should not be quoted as the cost of an upload. It falls when the card is the limit, because the thread then spends its time waiting.
- One application thread does drive all of it; "one core" would be wrong.

## Reading it

- **One client thread uploads to real S3 at 12.2 GB/s, which is the instance's 100 Gbit/s network card.** From 192 connections up the card is the limit. The whole machine is about three cores busy doing it; see the repeated runs above for why the thread's own figure is not the cost.
- **Checksums do not lower throughput at this scale.** With a CRC-64/NVME computed over every body and verified by S3 it is the same 12.2 GB/s; the checksum's cost shows up as a busier client thread.
- **S3 gives each connection about 80 MB/s for uploads**, so throughput is connections times 80 MB/s until the card is full. Eight connections are 0.6 GB/s; it takes about 150 to fill 100 Gbit/s.
- **GET is client-bound at 4.0 GB/s** on this CPU, lower than the 6.5 on the DGX: the cost is the kernel's copy into user buffers, and this core is slower at it.
- **HTTPS is client-bound at 2.9 GB/s for PUT and 2.5 for GET.** Graviton4 is the slowest CPU we have measured at AES (milestone 13), so this is the worst case for TLS, not the best.
- **Small requests cost the client 24 to 78 µs of CPU each**, a third to a half of what the same measurement showed over the internet to Linode, where idle timer ticks inflated it. Rates here are still the store's latency (30 to 130 ms per request), not the client's limit.

## Caveats

- One 8-second run per cell in every table except the repeated uploads, which are five runs per cell.
- GET at 128 connections could not be run: registering that many 8 MiB buffers exceeded the instance's locked-memory limit. That is a setting on the instance, not a limit of the client.
- A faster network card is needed to find where either S3 or the client stops for uploads. Nothing here says it is at 12.2 GB/s.
- One bucket, one prefix, one afternoon. S3's behaviour under sustained load, or with other key layouts, was not explored.
