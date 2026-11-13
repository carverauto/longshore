# Milestone 0 baseline — raw io_uring TCP, AArch64

Measured 2026-11-12 with `crates/longshore-bench/src/bin/tcp_raw.rs` (no protocol code).

- Hosts: two NVIDIA DGX Spark (host A -> host B), Linux 6.14.0-1015-nvidia, AArch64.
- Link: Mellanox mlx5 200 GbE, direct, MTU 9000 (192.0.2.1 -> 192.0.2.2).
- Thread under test: one io_uring thread pinned to CPU 19 (Cortex-X925, 4.0 GHz).
- Peer: `sink` (thread per connection, `recv(MSG_TRUNC)` so it does not copy) or `source` (thread per connection, blocking `write`).
- 5 s per run, one run per cell. GB = 10^9 bytes.
- `thread` = CPU time of the io_uring thread / wall time. `core` = busy share of CPU 19 from `/proc/stat`, which also counts interrupt work on that core.

Raw output: `m0-aarch64-dgx-spark.jsonl` (the main matrix; reruns and the multi-sender check below were captured from the terminal).

## Send

| Mode | Conns | Chunk | GB/s | thread | core | Note |
|---|---|---|---|---|---|---|
| unbatched (default ring, 1 syscall per op) | 8 | 1 MiB | 9.09 | 0.99 | 1.00 | CPU-bound |
| batched (single-issuer flags, fixed files) | 8 | 1 MiB | 9.37 | 1.00 | 1.00 | CPU-bound |
| batched | 8 | 16 KiB | 10.31 | 1.00 | 1.00 | CPU-bound |
| unbatched | 8 | 16 KiB | 5.94 | 1.00 | 1.00 | CPU-bound |
| zc (SEND_ZC, unregistered buffer) | 8 | 1 MiB | 13.51 | 0.45 | 0.67 | path-bound |
| zc-fixed (SEND_ZC + registered buffer) | 8 | 1 MiB | 13.62 | 0.14 | 0.40 | path-bound |
| zc-fixed | 8 | 8 MiB | 13.75 | 0.13 | 0.34 | path-bound |
| zc-fixed | 1 | 1 MiB | 8.28 | 0.11 | 0.27 | one flow |
| zc / zc-fixed | 8 | 16 KiB | 10.7 / 10.4 | 1.00 | 1.00 | kernel copied ~99% of sends |

## Receive

| Mode | Conns | Chunk | GB/s | thread | core |
|---|---|---|---|---|---|
| unbatched | 8 | 1 MiB | 7.19 | 0.99 | 1.00 |
| batched | 8 | 1 MiB | 7.22 | 0.95 | 1.00 |
| batched | 1 | 1 MiB | 7.75 | 0.97 | 0.98 |
| unbatched | 8 | 16 KiB | 5.44 | 1.00 | 1.00 |
| batched | 8 | 16 KiB | 5.68 | 1.00 | 1.00 |

## What this establishes

1. **Copying send costs a full core at about 9.4 GB/s; copying receive at about 7.2-7.8 GB/s.** The talk's figures were 6.9 and 5.6 on their hardware.
2. **Zero-copy send from registered buffers moves 13.6 GB/s using 14% of the thread and 40% of the core.** That is roughly 100 GB per thread-CPU-second.
3. **13.8 GB/s (110 Gbit/s) is a limit of this host pair, not of the sending core.** Two and four sender processes on separate cores still totalled 13.8 GB/s. The single-core zero-copy ceiling therefore cannot be measured here; dividing by core utilization suggests it is above 30 GB/s, which is consistent with the talk's 32 but is an extrapolation, not a measurement. Where the cap comes from (receiver, NIC, or platform) was not investigated.
4. **Registering the buffer cuts thread CPU by about 3x** at the same throughput (0.45 -> 0.14 of a core, 8 connections, 1 MiB).
5. **Zero-copy is useless for small sends.** At 16 KiB the kernel reported it had copied about 99% of sends (`IORING_NOTIF_USAGE_ZC_COPIED`), and throughput matched plain send. At 1 MiB and 8 MiB it reported zero copies.
6. **Batching and ring flags matter for small operations, not large ones.** At 16 KiB, 8 connections: 5.94 -> 10.31 GB/s. At 1 MiB the difference is within noise. The talk's 0.6 -> 5.6 GB/s receive chart is not reproduced by this workload; its "unoptimized" configuration is not described in the talk.

## Open observations

- **32 connections on one core is much slower than 8** for copying send (9.4 -> 3.9 GB/s at 1 MiB) and somewhat slower for receive. Not yet explained. It bears directly on how `connections_max` should be chosen.
- **Transient `ENOMEM`** from `SEND_ZC` and from buffer registration appeared in four cells immediately after runs that exited with millions of zero-copy sends still in flight. The same cells passed on rerun after a pause. Unverified hypothesis: locked-memory accounting is released only when the exited ring finishes tearing down. Consequence for `UringIo`: drain zero-copy notifications before dropping the ring.
- Single runs of 5 s; no variance measured.
- x86-64: the benchmark cross-compiles but has not been run. An x86-64 Linux host is needed.
