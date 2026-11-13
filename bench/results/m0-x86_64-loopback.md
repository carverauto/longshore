# Milestone 0 on x86-64 — loopback only, not a baseline

Measured 2026-11-12 on a shared x86-64 hypervisor host: Xeon Platinum 8358P, Linux 6.8.

This run exists to show the code works on x86-64. It is **not** a throughput budget:

- Loopback, because the host's only links are 1 GbE.
- The host was running 5 VMs with a load average of 11-16, and the benchmark thread was not pinned.
- 3 s per run, 4 connections, 1 MiB chunks, one run each.

| Command | Mode | GB/s | Note |
|---|---|---|---|
| send | unbatched | 7.33 | |
| send | batched | 6.99 | |
| send | zc-fixed | 6.33 | kernel copied every send (18,100 of 18,100): loopback has no zero-copy path |
| recv | unbatched | 6.77 | |
| recv | batched | 7.13 | |

Correctness on this host: `echo_uring` passed with plain and zero-copy sends at 1 MiB and 4 KiB chunks, including the timer and operation-limit checks, with allocation sealed after setup.

A real x86-64 baseline needs two x86-64 hosts joined by a link fast enough to load one core (25 GbE or better).
