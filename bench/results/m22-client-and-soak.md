# Milestone 22: the shareable client against S3, and a run with failures injected

Measured 2026-11-13 against a general-purpose S3 bucket in us-east-1, from the two instance types and images of milestone 20: `c8gn.24xlarge` (Graviton4, 300 Gbit/s card, first thread on CPU 2) and `m8azn.24xlarge` (AMD Zen 5, 200 Gbit/s card, first thread on CPU 50, on the card's NUMA node). 8 MiB objects, GB = 10^9 bytes, the preference for large-segment addresses on. Raw output: `m22-client-and-soak.jsonl`, where each line names its instance, its program (`bin`) and its label.

Two programs are compared. `s3_uring` drives the store directly, one store per thread, as in the earlier milestones. `client_s3` goes through `longshore::client`: caller threads borrow buffers from the client and call `store` and `fetch` on a shared handle, and the client's I/O threads do the work. One caller thread per I/O thread. I/O threads are pinned two CPUs apart. The client sends a CRC-64 checksum with every upload unless told not to. `s3_uring put` sends none.

Three runs of 8 s per cell. Every figure is the median, in GB/s. No operation failed in any run.

## Graviton4

| Request | I/O threads | Conns per thread | Direct | Client | Client, checksums off |
|---|---|---|---|---|---|
| PUT | 1 | 640 | 36.4, and 16.8 with a checksum | 16.2 | 37.6 |
| HTTPS PUT | 1 | 128 | 5.1 | 4.4 | 5.3 |
| HTTPS PUT | 4 | 128 | 19.5 | 17.1 | 20.9 |
| HTTPS PUT | 8 | 128 | 35.3 | 31.1 | 37.6 |
| GET | 1 | 128 | 4.7 | 4.6 | |
| GET | 4 | 128 | 15.3 | 15.6 | |
| GET | 8 | 128 | 26.2 | 26.7 | |
| HTTPS GET | 1 | 128 | 2.7 | 2.7 | |
| HTTPS GET | 4 | 128 | 10.1 | 10.1 | |
| HTTPS GET | 8 | 128 | 17.9 | 18.0 | |

## AMD Zen 5

| Request | I/O threads | Conns per thread | Direct | Client | Client, checksums off |
|---|---|---|---|---|---|
| PUT | 1 | 384 | 23.8, and 13.8 with a checksum | 13.6 | 25.0 |
| HTTPS PUT | 1 | 128 | 7.2 | 6.2 | 7.5 |
| HTTPS PUT | 2 | 128 | 15.3 | 13.1 | 15.5 |
| HTTPS PUT | 4 | 128 | 23.7 | 23.9 | 25.0 |
| GET | 1 | 128 | 7.9 | 8.0 | |
| GET | 2 | 128 | 14.3 | 14.5 | |
| GET | 4 | 128 | 22.1 | 22.4 | |
| HTTPS GET | 1 | 128 | 5.1 | 5.3 | |
| HTTPS GET | 2 | 128 | 9.2 | 9.5 | |
| HTTPS GET | 4 | 128 | 14.7 | 15.0 | |

Three AMD cells had one run well below the other two: direct GET on one thread (5.9 against 7.9), client HTTPS PUT on one thread (3.6 against 6.2 and 6.6) and client PUT with checksums off (21.9 against 25.0). The medians above are not affected. The cause was not looked for.

## What the figures say

- Handing requests to the client's I/O threads from other threads costs nothing that these runs can see. With the same work asked of both, the client is within 4% of the store driven directly in every cell, and above it in most.
- The client's default upload is slower than `s3_uring put` only because it sends a checksum. Plain PUT with a checksum is 16.2 GB/s through the client and 16.8 direct on Graviton4, and 13.6 against 13.8 on AMD. Over HTTPS the checksum costs 16% to 18% where the core is the limit.
- One I/O thread fills the Graviton4 card with plain PUT when checksums are off (37.6 GB/s). Eight I/O threads reach 31.1 GB/s for HTTPS PUT with checksums and 37.6 without, 26.7 for GET and 18.0 for HTTPS GET.
- On AMD, four I/O threads reach the same 24 to 25 GB/s over HTTPS that one thread reaches over plain HTTP. That is 190 to 200 Gbit/s on a 200 Gbit/s card.

## A 30 minute run with failures injected

On the Graviton4 instance: four I/O threads with 64 connections each, four caller threads, HTTPS PUT of 8 MiB objects with checksums, for 1,800 s. While it ran, a script on the same machine:

- every 40 s closed every TCP connection to port 443 with `ss -K` (43 times),
- every fourth time also dropped all outgoing packets to port 443 for 5 s with an `iptables` rule (10 times),
- every 120 s rewrote the client's credentials file from the instance role. The client reread the file every 60 s.

`ss -K` was checked afterwards on the same machine: it aborts an open connection to S3.

| | |
|---|---|
| Objects stored | 3,365,488 |
| Operations that returned an error to the caller | 0 |
| Throughput, whole run | 15.7 GB/s |
| Throughput per 30 s window | 13.0 to 16.5 GB/s, median 16.2. The ten windows with a 5 s packet drop are the ten lowest, 13.0 to 13.6 |
| Open file descriptors, sampled every 30 s | 16 at every sample |
| Resident memory, sampled every 30 s | 6,151 MB at every sample |
| Credentials adopted from the file | 4, one per I/O thread. No reload failed |

The per-window samples are in `m22-soak-progress.txt` and the time of each injected failure in `m22-soak-faults.txt`.

## What this does not show

- The run breaks connections. It does not make an I/O thread or its ring fail, and it does not shut the client down with work in flight. Those paths are tested in the simulator, and shutdown also on real kernels by `client_uring`, but not against a store.
- The role's keys changed at most once during the run, so the reload path mostly reread unchanged keys. Replacing keys that the store has stopped accepting is covered by `client_uring --rotate` against the in-tree server.
- Thirty minutes, one instance type, uploads only.
- Requests that failed and were retried inside the client are not counted here. Only what the caller saw is.
- One caller thread per I/O thread. Many callers sharing few I/O threads were not measured.
