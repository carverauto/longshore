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
- Runs of the same cell differ by up to 70%. That is far more than on the 100 Gbit/s instance in milestone 14, where they differed by a few percent. The cause was not found. One thing seen and not followed up: connections to S3 negotiated different segment sizes, some near 1,450 bytes and some near 8,950, and neither card segments in hardware (`tx-tcp-segmentation: off [fixed]`), so the kernel's per-byte cost depends on which addresses a run happened to connect to.
- PUT with a checksum is bound by the client's core at about 12 GB/s on Graviton4. On the 100 Gbit/s instance the card hid this.
- The AMD instance uploads at less than half the Graviton4 rate with its core full. A profile of the client's core during a plain upload shows it almost entirely in the kernel's transmit path: segmenting in software (`skb_segment`, `skb_zerocopy_clone`), allocating packet buffers, and the ENA driver's transmit routine. Why the same path costs so much less on the Graviton4 instance was not found.
- HTTPS PUT on the AMD instance is 3.1 GB/s although this processor seals TLS records at 25 GiB/s (milestone 13). In a profile, AES-GCM is 12% of the client's core and the kernel's transmit path is most of the rest. So HTTPS upload here is limited by the same transmit cost as plain upload, not by the cipher.
- GET is faster on the AMD instance, 6.8 GB/s plain and 4.5 GB/s over HTTPS against 3.9 and 2.5 on Graviton4. Receiving is bound by the kernel's copy and, over HTTPS, by the cipher, and this processor is faster at both.
- NUMA placement matters on the AMD instance. With the client pinned to a core on the other node from the network card, plain PUT at 256 connections ran at 3.8 to 4.9 GB/s. On the card's node it ran at 14.1 GB/s. These were single 6 s runs.

## Caveats

- A first set of runs had both instances writing to the same bucket prefix at once. Requests were retried and several runs failed, so that set was discarded and everything above was run again one instance at a time.
- The spread between runs means the medians are a rough guide. Finding its cause needs runs that record each connection's segment size and remote address.
- The AMD result describes this instance type and this driver. It does not say what the client does on x86-64 with a card that segments in hardware.
- Two short profiles, one per finding, on the AMD instance only. No profile was taken on Graviton4.
