# Milestone 29: TLS through AWS-LC, ring or rustls

Measured 2026-11-24 in a Linux container (Linux 7.2, OrbStack) on an Apple M3 Max: 4 vCPUs. GB = 10^9 bytes. Change `add-tls-providers` (issue #58).

## The record layer alone

`cargo bench -p longshore-bench --bench tls_providers`, pinned to CPU 3: 512 records of 16 KiB (8 MiB of application data) with AES-128-GCM keys from a real handshake, the send buffer and the destination reused. Opening is the difference between "seal then open" and "seal".

| Provider | Seal | Open | Seal then open |
|---|---|---|---|
| `aws-lc` (no copy) | 10.2 GB/s | 11.1 GB/s | 5.3 GB/s |
| `ring` (one copy) | 6.8 GB/s | 6.6 GB/s | 3.4 GB/s |
| `rustls` (rustls's record layer) | 6.2 GB/s | 6.7 GB/s | 3.2 GB/s |

AWS-LC runs its 8-way AES-GCM here (milestone 28). ring and rustls's record layer use the same ring cipher; rustls seals about 10% slower than ring, the cost of its buffer per record and second copy, and opens at the same speed, since it opens in place.

## HTTPS over loopback

`http_uring put|get --provider P` against the `http_uring sink` (rustls with AWS-LC, the same for every provider), 8 connections, 8 MiB bodies, 5 s, the client on CPU 3 and the sink on CPUs 0 to 2. Three runs per cell, medians. CPU per GB is the client thread's.

| | GB/s (range) | Client CPU per GB | Against plain |
|---|---|---|---|
| PUT, plain | 12.1 (11.5 to 12.9) | 82 ms | |
| PUT, `aws-lc` | 7.20 (6.98 to 7.46) | 139 ms | +57 ms |
| PUT, `ring` | 5.14 (5.03 to 5.26) | 195 ms | +113 ms |
| PUT, `rustls` | 4.61 (4.60 to 4.74) | 217 ms | +135 ms |
| GET, plain | 4.08 (4.07 to 4.13) | 245 ms | |
| GET, `aws-lc` | 2.99 (2.99 to 3.00) | 333 ms | +88 ms |
| GET, `ring` | 2.59 (2.55 to 2.61) | 385 ms | +140 ms |
| GET, `rustls` | 2.49 (2.43 to 2.59) | 402 ms | +157 ms |

What TLS adds to an upload is about twice as much with ring as with AWS-LC, and a little more again with rustls's record layer; on download the order is the same with smaller gaps, since the receive copy dominates. AWS-LC stays the default.

## What this does not tell us

- x86-64, other AArch64 CPUs and real stores: deferred to cloud hosts (tasks 5.2 and 5.3).
- This session ran faster than milestone 28's: plain PUT was 12.1 here against 9.9 there with the same code. Compare providers within this table only.
