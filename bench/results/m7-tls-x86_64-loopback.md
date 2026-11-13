# Milestone 7 — HTTPS PUT and GET, x86-64, loopback only

Measured 2026-11-12 with `http_uring` on one shared x86-64 hypervisor host (Xeon Platinum 8358P, Ice Lake, Linux 6.8), client and sink on the same host over loopback, 8 connections, 8 MiB objects, 4 s per run, two runs per cell (both shown). The host runs production guests (load average about 10 of 64 CPUs) and nothing was pinned. GB = 10^9 bytes. Raw output: `m7-tls-x86_64-loopback.jsonl`.

| Request | TLS | Seal batch | GB/s | Thread CPU | GB per thread-CPU-second |
|---|---|---|---|---|---|
| PUT | no | - | 5.35, 5.69 | 1.00 | 5.4, 5.7 |
| PUT | yes | 256 KiB | 1.70, 1.77 | 0.63 | 2.7, 2.9 |
| PUT | yes | 1 MiB | 1.83, 1.70 | 0.65 | 2.7, 2.7 |
| GET | no | - | 6.24, 6.67 | 1.00 | 6.3, 6.7 |
| GET | yes | - | 3.14, 3.11 | 0.99 | 3.2, 3.1 |

## Reading it

- **This is not a network measurement.** Loopback has no zero-copy send: the kernel copies every byte, and the client thread pays for it. Plain PUT saturates the client core at about 5.5 GB/s here, against 13.4 GB/s at 21% of a core on the AArch64 pair with a real 200 GbE link. The x86-64 network budget is still open.
- **TLS PUT costs the client about 2.7 GB per CPU-second on loopback**, roughly half the plain figure. Subtracting the plain cost leaves about 190 ms of CPU per GB for sealing, which would be about 5.3 GB/s for the cipher alone: half of the 11.2 GB/s the microbenchmark measured on this CPU model. A busy shared host and a cold cache between the seal and the kernel's copy are the likely reasons, but that was not investigated.
- **TLS PUT throughput is again limited by the sink** (client thread at 63 to 68%), as on AArch64.
- **TLS GET is client-bound at 3.1 GB/s**, about half of plain GET, the same ratio as on AArch64 (3.5 against 6.8).
- The two seal batch sizes cost the same CPU per byte here; 256 KiB needs five times the system calls (47 against 10 per request).

## Caveats

- Shared production hypervisor, unpinned, short runs: treat every figure as ±10% at best.
- Loopback only, so nothing here says what x86-64 does with a real NIC and zero-copy send.
- TLS PUT against a sink that is not the bottleneck is still unmeasured on either architecture.
