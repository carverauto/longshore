# Milestone 7 — HTTPS PUT and GET, AArch64

Measured 2026-11-12 with `http_uring` between host A and host B (DGX Spark, Linux 6.14, 200 GbE), one client io_uring thread pinned to CPU 19 (Cortex-X925), 8 MiB objects, 5 s per run, one run per cell. GB = 10^9 bytes.

Client: handshake by rustls, then every record sealed and opened by `longshore::tls` (AES-128-GCM through AWS-LC's scatter/gather calls). Peer: `http_uring sink --tls-cert`, a thread-per-connection rustls server.

| Request | TLS | Seal batch | Conns | GB/s | Thread CPU | GB per thread-CPU-second |
|---|---|---|---|---|---|---|
| PUT | no | - | 8 | 13.4 | 0.21 | 63.2 |
| PUT | yes | 64 KiB | 8 | 3.32 | 0.68 | 4.9 |
| PUT | yes | 256 KiB | 8 | 2.04 | 0.35 | 5.8 |
| PUT | yes | 1 MiB | 8 | 3.50 | 0.53 | 6.6 |
| PUT | yes | 8 MiB | 8 | 3.62 | 0.90 | 4.0 |
| PUT | yes | 256 KiB | 16 | 4.12 | 0.73 | 5.7 |
| GET | no | - | 8 | 6.78 | 0.95 | 7.1 |
| GET | yes | - | 1 | 3.47 | 0.91 | 3.8 |
| GET | yes | - | 8 | 3.51 | 0.98 | 3.6 |

## Reading it

- **TLS PUT throughput here is limited by the sink, not the client.** The client thread was never saturated (35% to 90%), and one connection carried only 0.26 GB/s, which is the rustls server's read path. The GB/s column for TLS PUT therefore understates the client.
- **The client's own cost for TLS PUT is 4.0 to 6.6 GB per CPU-second**, best at a 1 MiB seal batch. Taken as a ceiling, that is about 6.6 GB/s on one core, against 8.3 GB/s for the cipher alone in the microbenchmark and 7.4 GB/s for the talk's cipher. It is an estimate from CPU time, not a measured throughput.
- **Seal batch size matters.** Sealing 8 MiB at a time costs about 60% more CPU per byte than 1 MiB, because the send buffer no longer fits in cache; 64 KiB costs more again through extra system calls. The default is now 1 MiB.
- **TLS GET is client-bound at 3.5 GB/s** (98% of the core), about half of plain GET. Decrypting adds roughly 140 ms of CPU per GB on top of the kernel's receive copy.
- **Plain PUT is unchanged from milestone 3** (13.4 GB/s at the host pair's cap), so routing I/O through the transport layer did not cost the plain path anything measurable.

## Against real services

- One HTTPS GET through the full stack on io_uring succeeded against `us-ord-10.linodeobjects.com`, `s3.amazonaws.com` and `storage.googleapis.com`.
- `s3_uring selftest` over HTTPS against the Linode bucket passed with an 8 MiB object: store, conditional store, fetch, ranged fetch, erase, with every byte compared and allocation sealed outside the handshake.
- CPU measurements from host A to Linode over the internet varied between 267 and 484 ms per GB for the same configuration and were discarded as noise.

## A bug the real servers found

The first real HTTPS attempt hung. rustls accepts at most about 4 KiB per call, and the handshake wrapper assumed one call consumed a whole record, so the tail of a large certificate record was dropped and both sides waited for each other. The simulator's test certificate was small enough to hide it. Fixed, with a regression test that uses a certificate over 8 KiB and fails on the old code.

## Caveats

- Single 5 s runs. The 256 KiB, 8-connection PUT row (2.04 GB/s) is out of line with its neighbours and was not repeated.
- x86-64 TLS was measured on loopback only: `m7-tls-x86_64-loopback.md`.
- AES-256-GCM, which Linode negotiates by default, was not measured separately on the LAN.
