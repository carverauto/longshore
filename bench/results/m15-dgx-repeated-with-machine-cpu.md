# Milestone 15 — DGX pair, repeated runs with whole-machine CPU

Measured 2026-11-13 with `http_uring` between host A (client) and host B (peer), DGX Spark, AArch64, Linux 6.14, both 200 GbE interfaces. One client io_uring thread pinned to CPU 19 (Cortex-X925), 8 MiB objects, 8 s per run, **five runs per cell**. GB = 10^9 bytes. Raw output: `m15-dgx-repeated-with-machine-cpu.jsonl`.

These repeat the configurations of milestones 10 and 11 for two reasons: those were single runs, and they measured only the client's thread and the core it was pinned to. Kernel network work also runs in interrupt context on other cores. Here each run also records how busy the whole 20-core machine was, from `/proc/stat`. With nothing of ours running the machine was 0.23 cores busy.

The peer is the program's own sink, which discards what it receives. It is not an object store.

Median, with the lowest and highest in brackets:

| Request | TLS | Checksum | Conns | GB/s | Client thread | Pinned core | Whole machine, cores |
|---|---|---|---|---|---|---|---|
| PUT | no | no | 16 | 24.26 (24.16 to 24.44) | 0.70 | 0.88 | 2.15 (2.07 to 2.19) |
| PUT | no | no | 32 | 24.50 (24.46 to 24.56) | 0.82 | 0.96 | 2.54 (2.40 to 2.62) |
| PUT | no | CRC-64/NVME | 16 | 14.69 (14.49 to 15.04) | 0.96 | 1.00 | 1.53 (1.51 to 1.58) |
| GET | no | no | 8 | 6.43 (5.90 to 6.70) | 0.96 | 0.98 | 1.52 (1.42 to 1.54) |
| PUT | yes | no | 48 | 5.81 (5.79 to 5.86) | 0.99 | 1.00 | 1.29 (1.28 to 1.34) |
| PUT, 8-way AES-GCM build | yes | no | 48 | 7.57 (7.42 to 7.66) | 0.99 | 1.00 | 1.32 (1.31 to 1.34) |
| PUT, 8-way AES-GCM build | yes | CRC-64/NVME | 48 | 6.04 (5.97 to 6.16) | 0.98 | 1.00 | 1.31 (1.27 to 1.35) |
| GET, 8-way AES-GCM build | yes | no | 16 | 4.10 (3.97 to 4.25) | 0.98 | 1.00 | 1.49 (1.39 to 1.61) |

## Reading it

- **The throughput figures of milestones 10 and 11 hold.** Every median is within 2.5% of the earlier single run except plain GET at 8 connections, 4% below its 6.70 GB/s, and the spread across five runs is under 4% except for GET.
- **Plain PUT costs the machine more than its pinned core.** At 24.3 GB/s (16 connections) the machine is about 2.2 cores busy, and at 24.5 GB/s (32 connections) about 2.5. The pinned core is nearly full, and about 1.3 and 1.6 cores of work run off it, mostly kernel transmit completions and acknowledgements. "One thread" is accurate; "one core" is not.
- **Where the client's own core is the limit, the overflow is small.** Checksummed PUT, TLS and GET all fill the pinned core and use 0.3 to 0.5 of a core beyond it, of which 0.23 is the machine's idle load.
- **Per whole-machine core, plain PUT is about 12.6 GB/s and checksummed PUT about 11.3 GB/s** (24.3 over 2.15 less idle; 14.7 over 1.53 less idle). The plain figure is the one a "per core" comparison should use if kernel work elsewhere is counted.

## Caveats

- The talk this client follows reports its figures "per core". Its slides do not say whether kernel work on other cores was counted, so we cannot say which of our figures is the like-for-like one.
- The machines run other workloads; the idle figure was taken once, before the runs.
- Interrupts were left where the system put them. Steering the card's interrupts onto the client's core would make "one core" measurable directly, and would probably lower throughput.
