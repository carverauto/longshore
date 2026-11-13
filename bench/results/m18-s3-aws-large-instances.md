# Milestone 18: Amazon S3 from larger instances

Measured 2026-11-13 with `s3_uring` against a general-purpose S3 bucket in us-east-1, from two instances in the same region, one at a time:

- `c8gn.24xlarge`: Graviton4, 96 vCPUs, one NUMA node, 300 Gbit/s network card. Client thread pinned to CPU 2.
- `m8azn.24xlarge`: AMD EPYC 9R05 (Zen 5), 96 vCPUs, two NUMA nodes, 200 Gbit/s network card. Client thread pinned to CPU 50, on the card's NUMA node and not one of the cores that take its interrupts.

Both ran Ubuntu 24.04 with Linux 7.0 and the ENA driver. One client io_uring thread, 8 MiB objects, 8 s per run, five runs per cell. GB = 10^9 bytes. "Whole machine" is cores busy across the instance, from `/proc/stat`. Raw output: `m18-s3-aws-large-instances.jsonl`. Four runs printed no usable result line and are left out, so four cells have four runs.

The self-test passed over HTTP and HTTPS on both instances before the runs.

## Graviton4, 300 Gbit/s card

Median, with the lowest and highest in brackets.

| Request | Conns | GB/s | Client thread | Whole machine, cores busy |
|---|---|---|---|---|
| PUT | 384 | 29.2 (26.8 to 29.8) | 0.94 | 4.5 |
| PUT | 512 | 26.8 (22.7 to 29.6) | 1.00 | 6.2 |
| PUT | 640 | 29.2 (21.4 to 35.7) | 0.99 | 6.8 |
| PUT | 768 | 24.5 (21.1 to 36.5) | 0.96 | 5.3 |
| PUT with CRC-64/NVME | 256 | 11.7 (11.0 to 13.0) | 1.00 | 2.5 |
| PUT with CRC-64/NVME | 384 | 12.1 (9.4 to 13.4) | 1.00 | 2.5 |
| GET | 64 | 3.9 (3.8 to 4.2) | 0.98 | 1.1 |
| HTTPS PUT | 64 | 3.0 (2.9 to 3.3) | 0.98 | 1.1 |
| HTTPS PUT with CRC-64/NVME | 64 | 2.8 (2.7 to 3.0) | 0.91 | 1.0 |
| HTTPS GET | 64 | 2.5 (2.5 to 2.5) | 0.97 | 1.1 |

## AMD Zen 5, 200 Gbit/s card

| Request | Conns | GB/s | Client thread | Whole machine, cores busy |
|---|---|---|---|---|
| PUT | 192 | 11.4 (7.5 to 12.3) | 1.00 | 3.3 |
| PUT | 256 | 11.6 (7.6 to 14.4) | 1.00 | 3.2 |
| PUT | 384 | 9.6 (8.4 to 18.2) | 1.00 | 2.9 |
| PUT with CRC-64/NVME | 256 | 8.4 (7.4 to 11.0) | 1.00 | 2.5 |
| PUT with CRC-64/NVME | 384 | 8.0 (7.2 to 9.2) | 1.00 | 2.4 |
| GET | 128 | 6.8 (6.1 to 7.4) | 1.00 | 1.3 |
| HTTPS PUT | 64 | 3.1 (2.4 to 3.3) | 0.98 | 1.1 |
| HTTPS PUT with CRC-64/NVME | 64 | 2.5 (2.4 to 2.8) | 0.96 | 1.1 |
| HTTPS GET | 128 | 4.5 (4.2 to 4.9) | 1.00 | 1.3 |

Three requests out of 1,015,470 across both instances needed a second attempt.

## Reading it

- Plain PUT from one client thread reached a median of 29 GB/s on Graviton4, about 235 Gbit/s, with the thread close to fully busy. Single runs reached 36 GB/s, which is the 300 Gbit/s card. The client's core is now the usual limit for plain PUT to S3, and the card is only sometimes.
- Runs of the same cell differ by up to 73% on Graviton4 and up to 117% on the AMD instance, taking the gap between the fastest and slowest run as a share of the slowest, as milestone 14 does. That is far more than on the 100 Gbit/s instance in milestone 14, where they differed by a few percent. The cause is the segment size each connection negotiated, which depends on the S3 address it reached. The section after this one has the runs that show it.
- PUT with a checksum is bound by the client's core at about 12 GB/s on Graviton4. On the 100 Gbit/s instance the card hid this.
- The AMD instance uploads at less than half the Graviton4 rate with its core full. A profile of the client's core during a plain upload shows it almost entirely in the kernel's transmit path: segmenting in software (`skb_segment`, `skb_zerocopy_clone`), allocating packet buffers, and the ENA driver's transmit routine. This is the segment size again: with every connection on an address that takes large segments, this instance fills its card at 384 connections, and at 256 connections it is still bound by the client's core, at 18.8 to 19.2 GB/s.
- HTTPS PUT on the AMD instance is 3.1 GB/s although this processor seals TLS records at 25 GiB/s (milestone 13). In a profile, AES-GCM is 12% of the client's core and the kernel's transmit path is most of the rest. So HTTPS upload here is limited by the same transmit cost as plain upload, not by the cipher.
- GET is faster on the AMD instance, 6.8 GB/s plain and 4.5 GB/s over HTTPS at 128 connections, against 3.9 plain and 2.5 over HTTPS at 64 connections on Graviton4. Plain GET is bound by the kernel's copy, as the profiles in the next section show, and this processor copies faster. The HTTPS GET figures were not profiled, and their difference is not explained.
- NUMA placement matters on the AMD instance. With the client pinned to a core on the other node from the network card, plain PUT at 256 connections ran at 3.8 to 4.9 GB/s. On the card's node it ran at 14.1 GB/s. These were single 6 s runs.

## The cause of the spread

Taken later the same day, on instances of the same two types, with the client built from the same source plus a setting that fixes the address it connects to.

The S3 endpoint's name resolved to eight addresses. Front ends at some of them advertised a maximum segment size of 8,228 bytes, and at the others 1,432 bytes. The instances' MTU is 9,001. The resolver hands addresses out in rotation, so a run's connections were spread evenly over the eight, and how many of the eight took large segments changed from one lookup to the next. Neither card segments in hardware (`tx-tcp-segmentation: off [fixed]`), so the kernel cuts every send into segments in software, on the client's thread. A connection with small segments costs that thread several times more per byte.

Plain PUT on the AMD instance at 256 connections, by how the connections fell:

| Connections with large segments | Connections with small segments | GB/s |
|---|---|---|
| 192 | 64 | 13.7 |
| 160 | 96 | 11.4 to 11.6 |
| 128 | 128 | 10.1 to 10.9 |
| 256 (address fixed) | 0 | 18.8 to 19.2 |

With every connection fixed to one address that takes large segments, runs of the same cell agreed within a few percent:

| Instance | Request | Conns | GB/s | Client thread | What limits it |
|---|---|---|---|---|---|
| Graviton4 | PUT | 384 | 27.9 to 28.3 | 0.63 | The number of connections |
| Graviton4 | PUT | 512 | 34.9 to 36.7 | 0.62 | The number of connections |
| Graviton4 | PUT | 640 | 36.9 to 37.0 | 0.54 | The card, at about 295 Gbit/s |
| Graviton4 | PUT with CRC-64/NVME | 384 | 15.9 to 16.5 | 1.00 | The client's core |
| Graviton4 | GET | 128 | 4.8 | 1.00 | The client's core |
| Graviton4 | HTTPS PUT, 4 MiB send batch | 128 | 4.9 | 0.99 | The client's core, 72% of it in AES-GCM |
| AMD | PUT | 256 | 18.8 to 19.2 | 0.98 | The client's core |
| AMD | PUT | 384 | 23.3 to 23.7 | 0.70 | The card, at about 188 Gbit/s |
| AMD | PUT with CRC-64/NVME | 384 | 13.8 | 1.00 | The client's core |
| AMD | GET | 128 | 7.8 | 1.00 | The client's core |
| AMD | HTTPS PUT, 4 MiB send batch | 128 | 6.7 to 7.1 | 0.88 | Mostly the cipher: 55% of the client's core is in AES-GCM |
| AMD | HTTPS PUT, 4 MiB send batch | 256 | 6.8 to 6.9 | 0.99 | The client's core |
| AMD | HTTPS GET | 128 | 4.8 | 1.00 | The client's core |

Rows with one figure are single runs of 12 s with `perf` attached. The others are two or three runs of 8 s.

What the profiles show:

- Plain PUT has nothing left to find on these instances. At 640 connections on Graviton4 and 384 on the AMD instance the card is full with the client's thread partly idle. At 256 connections the AMD instance is bound by the client's core.
- HTTPS PUT is bound by the cipher once segments are large. The client seals up to a set number of bytes and then sends them in one operation. With 4 MiB per send instead of the default 1 MiB, the profiled runs gave 7.1 against 6.3 GB/s on the AMD instance and 4.9 against 4.3 on Graviton4. A 2 MiB run on the AMD instance gave 5.9, below both, so the best size needs a proper sweep.
- GET spends about half of the client's core in the kernel's copy to our buffers (49% on AMD, 54% on Graviton4), 17 to 18% in the CRC-64, and 9% (AMD) to 17% (Graviton4) in `check_heap_object`, a check the kernel makes on that copy.

The client does not yet choose addresses by segment size. Until it does, a run through the resolver gets the mixed figures in the first two tables.

## Caveats

- A first set of runs had both instances writing to the same bucket prefix at once. Requests were retried and several runs failed, so that set was discarded and everything above was run again one instance at a time.
- The spread between runs means the medians in the two tables are a rough guide to what a client gets when it spreads connections over every address S3 returns. They are not what the client or the instance can do.
- The runs in "The cause of the spread" are two or three per row, or a single run with a profiler attached, of 8 to 12 s each. They are enough to show the cause. They are not medians of five.
- The AMD result describes this instance type and this driver. It does not say what the client does on x86-64 with a card that segments in hardware.
