# Milestone 3 — HTTP PUT and GET throughput, AArch64

Measured 2026-11-12 with `crates/longshore-bench/src/bin/http_uring.rs`.

- Same hosts and link as milestone 0: host A -> host B, DGX Spark, Linux 6.14, 200 GbE, MTU 9000.
- Client: one io_uring thread pinned to CPU 19 (Cortex-X925), plain HTTP, no checksums, no TLS. Request bodies of 64 KiB or more are sent zero-copy from registered buffers.
- Peer: `http_uring sink`, a thread-per-connection HTTP responder that discards PUT bodies in the kernel (`MSG_TRUNC`) and serves filler for GET. It is not the in-tree test server, which is single-threaded and would be the bottleneck.
- 5 s per run, one run per cell. GB = 10^9 bytes. `thread` and `core` as in milestone 0.

| Request | Conns | Object | GB/s | thread | core | syscalls per request |
|---|---|---|---|---|---|---|
| PUT | 1 | 8 MiB | 6.99 | 0.08 | 0.22 | 8.0 |
| PUT | 2 | 8 MiB | 9.42 | 0.11 | 0.29 | 7.7 |
| PUT | 4 | 8 MiB | 12.34 | 0.19 | 0.43 | 7.3 |
| PUT | 8 | 8 MiB | 13.38 | 0.20 | 0.48 | 7.1 |
| PUT | 8 | 64 KiB | 4.63 | 0.50 | 0.58 | 2.4 |
| GET | 1 | 8 MiB | 6.00 | 0.75 | 0.76 | 31.4 |
| GET | 4 | 8 MiB | 6.06 | 0.95 | 0.98 | 2.1 |
| GET | 8 | 8 MiB | 6.55 | 0.96 | 0.98 | 1.4 |

## Reading it

- **PUT at 8 connections reaches the same 13.4-13.8 GB/s cap as raw zero-copy TCP in milestone 0**, using 20% of the thread and 48% of the core. The HTTP layer is not what limits it; the host pair is. The talk's 25.2 GB/s cannot be confirmed or refuted on this hardware.
- **HTTP adds little over raw TCP.** Raw zero-copy send was 13.6 GB/s at 14% thread / 40% core; HTTP PUT is 13.4 GB/s at 20% / 48%.
- **GET is CPU-bound at about 6.5 GB/s**, against 7.2 for raw copying receive. The talk's figure is 4.7. The cost is the kernel copying into user buffers, as the talk says.
- **Small objects are a different regime.** At 64 KiB (the zero-copy threshold) throughput is 4.6 GB/s and per-request overhead dominates.
- All runs executed with heap allocation sealed after setup; an allocation would have aborted the process.

## Caveats

- Single 5 s runs, no variance measured.
- Connections 16 and 32 were not run; milestone 0 found copying send degrades sharply at 32 connections per core.
- No x86-64 run: no pair of x86-64 hosts with a fast link is available. Akamai/Linode plans top out at 16 Gbit/s outbound (checked with `linode-cli linodes types`), below what this pair already carries.
