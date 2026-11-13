# Milestone 10 — HTTP PUT and GET over both links of the DGX pair

Measured 2026-11-13 with `http_uring` between host A (client) and host B (peer), DGX Spark, AArch64, Linux 6.14. One client io_uring thread pinned to CPU 19 (Cortex-X925), 8 MiB objects, plain HTTP, 8 s per run, one run per cell. GB = 10^9 bytes. Raw output: `m10-http-dual-link-aarch64-dgx-spark.jsonl`.

## Why two links

Earlier runs stopped at 13.4 to 13.8 GB/s (110 Gbit/s) however many cores sent. The cause is the machine, not the client: each DGX Spark presents its one 200 GbE port as two network interfaces, each on its own PCIe link of 4 lanes at 32 GT/s, about 128 Gbit/s raw. Everything before this used the first interface only. Here the second interface on each host has its own address (198.51.100.0/24, MTU 9000) and the client spreads its connections across both: connection i goes to address i mod 2 (`--addr A,B`).

The peer is `http_uring sink`: a thread per connection that reads each request and discards the body in the kernel. It is not an object store. These figures are what the client costs, not what any store accepts.

## Results

| Request | Checksum | Links | Conns | GB/s | Gbit/s | Client thread | Whole core | GB per thread-CPU-second |
|---|---|---|---|---|---|---|---|---|
| PUT | no | 2 | 4 | 17.61 | 141 | 0.34 | 0.61 | 52.4 |
| PUT | no | 2 | 8 | 22.77 | 182 | 0.59 | 0.83 | 38.4 |
| PUT | no | 2 | 12 | 23.93 | 191 | 0.60 | 0.82 | 39.8 |
| PUT | no | 2 | 16 | 24.24 | 194 | 0.64 | 0.84 | 38.1 |
| PUT | no | 2 | 24 | 24.44 | 196 | 0.77 | 0.93 | 31.6 |
| PUT | no | 2 | 32 | 24.46 | 196 | 0.84 | 0.96 | 29.2 |
| PUT | CRC-64/NVME | 2 | 8 | 14.91 | 119 | 0.95 | 0.99 | 15.7 |
| PUT | CRC-64/NVME | 2 | 16 | 14.38 | 115 | 0.96 | 1.00 | 15.0 |
| PUT | CRC-64/NVME | 2 | 24 | 14.67 | 117 | 0.96 | 1.00 | 15.2 |
| PUT | CRC-64/NVME | 1 | 8 | 12.68 | 101 | 0.77 | 0.87 | 16.5 |
| GET | no | 2 | 8 | 6.70 | 54 | 0.96 | 0.98 | 7.0 |
| GET | no | 2 | 16 | 6.21 | 50 | 0.97 | 1.00 | 6.4 |

"Client thread" is the CPU time of the client's thread. "Whole core" is how busy CPU 19 was, which also counts the kernel's network work that ran there on the thread's behalf.

## Reading it

- **Plain PUT fills the link.** 24.2 to 24.5 GB/s is 194 to 196 Gbit/s on a 200 Gbit/s port. From 16 connections up the wire is the limit, with the core 84 to 96% busy. The talk's figure for the same configuration is 25.2 GB/s per core; on this hardware the two are within 3%, and ours is capped by the link.
- **The core is not far from full either.** At 16 connections the core has 16% left, so the client's own ceiling on this CPU is probably in the high 20s of GB/s, not far above the link. That is an extrapolation; a faster link is needed to measure it.
- **More connections cost more CPU per byte.** 38 GB per thread-CPU-second at 8 to 16 connections falls to 29 at 32. Throughput does not drop, so the earlier concern about 32 connections is a CPU cost, not a stall.
- **Checksummed PUT is CPU-bound at 14.4 to 14.9 GB/s.** Every body is checksummed with CRC-64/NVME before it is sent, on the same core. The talk gives 4.3 GB/s as its CRC64 throughput and names it as the end-to-end ceiling with checksums on. This is about 3.4 times that, and it is a real ceiling: the core is full and the link is not.
- **The checksum costs about 40 ms of CPU per GB here** (15.2 against 38 GB per thread-CPU-second), close to the 33 ms measured in milestone 5.
- **GET is CPU-bound at 6.2 to 6.7 GB/s**, the same as with one link: the kernel copying into user buffers is the cost, as before. The talk's figure is 4.7.

## HTTPS over both links

Same setup, with TLS 1.3 (AES-128-GCM): handshake by rustls, every record sealed and opened by `longshore::tls`. The peer is `http_uring sink --tls-cert`, a rustls server with a thread per connection. Raw output: `m10-https-dual-link-aarch64-dgx-spark.jsonl`. PUT was run two or three times per connection count.

| Request | Checksum | Conns | GB/s | Client thread | GB per thread-CPU-second |
|---|---|---|---|---|---|
| PUT | no | 8 | 3.76 | 0.59 | 6.3 |
| PUT | no | 16 | 3.73, 3.87, 5.90 | 0.57 to 0.99 | 5.9 to 6.6 |
| PUT | no | 24 | 5.77, 5.89 | 0.97 | 5.9 to 6.1 |
| PUT | no | 32 | 5.78, 5.86, 5.82 | 0.99 | 5.9 |
| PUT | no | 48 | 5.85, 5.79 | 0.99 | 5.9 |
| PUT | no | 64 | 5.78, 5.78 | 0.99 | 5.9 |
| PUT | CRC-64/NVME | 16 | 4.95 | 0.99 | 5.0 |
| PUT | CRC-64/NVME | 48 | 4.89 | 0.98 | 5.0 |
| GET | no | 8 | 3.19 | 0.97 | 3.3 |
| GET | no | 16 | 3.41 | 0.99 | 3.5 |

- **HTTPS PUT is CPU-bound at 5.8 to 5.9 GB/s** from 24 connections up. This is the first run where the client, not the peer, is the limit, so it is a measured ceiling. With fewer connections the peer limits it (the client thread is 57 to 61% busy), which is also why the 16-connection figure varies between runs.
- **That is below the cipher alone.** AES-GCM sealing measured 8.3 GB/s on this core in milestone 6; in the full path a GB costs about 170 ms of CPU against 120 ms for the cipher by itself. The talk gives 7.4 GB/s for its cipher and no end-to-end TLS figure. So on TLS we are not ahead: our end-to-end number is below their cipher-only number, and the 50 ms per GB around the cipher is the place to look.
- **Checksum and encryption together: 4.9 GB/s.**
- **HTTPS GET is CPU-bound at 3.2 to 3.4 GB/s**, as with one link.

## Caveats

- One 8-second run per cell.
- The peer discards data; nothing here says what a real object store would accept.
- The talk's hardware and exact method are not known to us beyond its slides, so the comparison shows where we stand, not a controlled head-to-head.
- The second interface's address was added by hand for this run and is not part of either host's configuration.
