# Milestone 20: the large-segment preference, more than one client, and other clients

Measured 2026-11-13 with `s3_uring` against a general-purpose S3 bucket in us-east-1, from two instances in the same region:

- `c8gn.24xlarge`: Graviton4, 96 vCPUs, one NUMA node, 300 Gbit/s network card. Client thread pinned to CPU 2.
- `m8azn.24xlarge`: AMD EPYC 9R05 (Zen 5), 96 vCPUs, two NUMA nodes, 200 Gbit/s network card. Client thread pinned to CPU 50, on the card's NUMA node.

Both ran Ubuntu 24.04 with Linux 7.0 and the ENA driver, which does not segment in hardware (`tcp-segmentation-offload: off`). MTU 9001. 8 MiB objects, GB = 10^9 bytes. "Whole machine" is cores busy across the instance, from `/proc/stat`. Raw output: `m20-preference-and-comparison.jsonl`, where each line names its instance and round.

Milestone 18 found that S3's name resolves to addresses whose connections get either large or small TCP segments, and that the small ones cost the client's thread several times more per byte. This milestone measures the setting built to deal with that (`S3_PREFER_LARGE_SEGMENTS=2`: prefer large-segment addresses, keep at least two in use), then what several clients side by side do, then two other S3 clients on the same machines.

## One client thread, preference off and on

Five runs of 8 s per row, one instance at a time. The batch is how many bytes of TLS records the client seals before each send.

### Graviton4, 300 Gbit/s card

| Request | Conns | Preference | GB/s, median (lowest to highest) | Client thread | Whole machine, cores busy |
|---|---|---|---|---|---|
| PUT | 384 | Off | 28.0 (22.3 to 29.5) | 0.93 | 4.8 |
| PUT | 384 | On | 29.5 (29.3 to 29.5) | 0.71 | 2.6 |
| PUT | 640 | Off | 23.2 (19.4 to 28.2) | 1.00 | 5.1 |
| PUT | 640 | On | 36.7 (36.3 to 36.9) | 0.51 | 3.9 |
| PUT with CRC-64/NVME | 384 | Off | 11.4 (10.1 to 14.0) | 1.00 | 2.4 |
| PUT with CRC-64/NVME | 384 | On | 16.4 (16.2 to 16.6) | 1.00 | 1.9 |
| GET | 128 | Off | 4.2 (3.7 to 4.3) | 1.00 | 1.2 |
| GET | 128 | On | 4.4 (4.3 to 4.4) | 0.99 | 1.2 |
| HTTPS PUT | 128 | Off, 1 MiB batch | 3.0 (2.4 to 3.5) | 0.99 | 1.2 |
| HTTPS PUT | 128 | On, 1 MiB batch | 4.0 (3.9 to 4.1) | 0.99 | 1.1 |
| HTTPS PUT | 128 | On, 2 MiB batch | 4.6 (4.5 to 4.6) | 0.99 | 1.2 |
| HTTPS PUT | 128 | On, 4 MiB batch | 5.0 (4.9 to 5.0) | 0.99 | 1.2 |
| HTTPS PUT | 128 | On, 8 MiB batch | 4.0 (4.0 to 4.1) | 0.99 | 1.2 |
| HTTPS GET | 128 | Off | 2.4 (2.4 to 2.5) | 1.00 | 1.1 |
| HTTPS GET | 128 | On | 2.7 (2.6 to 2.7) | 1.00 | 1.1 |

### AMD Zen 5, 200 Gbit/s card

| Request | Conns | Preference | GB/s, median (lowest to highest) | Client thread | Whole machine, cores busy |
|---|---|---|---|---|---|
| PUT | 256 | Off | 10.6 (7.9 to 14.1) | 1.00 | 3.4 |
| PUT | 256 | On | 18.8 (6.8 to 19.4) | 0.97 | 2.8 |
| PUT | 384 | Off | 11.8 (7.1 to 24.6) | 1.00 | 3.2 |
| PUT | 384 | On | 22.4 (21.1 to 24.1) | 0.80 | 3.4 |
| PUT with CRC-64/NVME | 384 | Off | 8.2 (7.3 to 9.2) | 1.00 | 2.5 |
| PUT with CRC-64/NVME | 384 | On | 13.3 (13.1 to 13.5) | 1.00 | 2.2 |
| GET | 128 | Off | 7.2 (6.5 to 7.2) | 1.00 | 1.4 |
| GET | 128 | On | 7.9 (7.8 to 7.9) | 1.00 | 1.2 |
| HTTPS PUT | 128 | Off, 1 MiB batch | 3.5 (3.5 to 4.2) | 0.99 | 1.2 |
| HTTPS PUT | 128 | On, 1 MiB batch | 5.1 (5.0 to 5.1) | 0.99 | 1.2 |
| HTTPS PUT | 128 | On, 2 MiB batch | 5.6 (5.5 to 5.8) | 0.99 | 1.3 |
| HTTPS PUT | 128 | On, 4 MiB batch | 7.0 (6.9 to 7.1) | 0.99 | 1.4 |
| HTTPS PUT | 128 | On, 8 MiB batch | 5.8 (3.7 to 5.8) | 0.99 | 1.4 |
| HTTPS GET | 128 | Off | 4.5 (4.4 to 4.6) | 1.00 | 1.3 |
| HTTPS GET | 128 | On | 5.1 (5.0 to 5.1) | 0.99 | 1.2 |

Reading it:

- With the preference on, plain PUT fills the Graviton4 instance's card at 36.7 GB/s (about 294 Gbit/s) with half of the client's thread idle, and runs agree within 2%. With it off the median at the same 640 connections is 23.2 GB/s and runs differ by 45%.
- On the AMD instance plain PUT at 384 connections is 22.4 GB/s with the thread at 0.80, against 11.8 with the preference off.
- PUT with a checksum, GET and both HTTPS directions are bound by the client's core with the preference on or off. The preference still helps each of them, by 5% for GET on Graviton4 and by 44% to 62% for PUT with a checksum.
- One AMD run with the preference on gave 6.8 GB/s (PUT, 256 connections). Every address in that run's DNS answer had small segments, so there was nothing to prefer. The client was then changed to ask DNS again sooner in that case. The same cell with the rebuilt client is in the next section.
- The 8 MiB batch is worse than 4 MiB on both instances. An 8 MiB batch is a whole object, so nothing is sent until all of it is sealed.

## The rebuilt client, and the batch size

Later the same session, with the client that compares segment sizes with what the path could carry and refreshes DNS early when every address is small. Preference on throughout. The 3 MiB and 4 MiB rows are eight runs each, three in one round and five alternating in another.

### Graviton4

| Request | Conns | Batch | Runs | GB/s, median (lowest to highest) |
|---|---|---|---|---|
| PUT | 640 |  | 5 | 36.4 (36.3 to 36.6) |
| HTTPS PUT | 128 | 3 MiB | 8 | 5.2 (5.2 to 5.3) |
| HTTPS PUT | 128 | 4 MiB | 8 | 5.0 (4.9 to 5.0) |
| HTTPS PUT | 128 | 6 MiB | 3 | 4.5 (4.4 to 4.5) |
| HTTPS GET | 128 | 4 MiB | 5 | 2.8 (2.7 to 2.8) |

### AMD

| Request | Conns | Batch | Runs | GB/s, median (lowest to highest) |
|---|---|---|---|---|
| PUT | 256 |  | 5 | 19.5 (19.2 to 19.6) |
| HTTPS PUT | 128 | 3 MiB | 8 | 7.3 (7.2 to 7.6) |
| HTTPS PUT | 128 | 4 MiB | 8 | 7.0 (6.9 to 7.1) |
| HTTPS PUT | 128 | 6 MiB | 3 | 6.3 (6.3 to 6.6) |
| HTTPS GET | 128 | 4 MiB | 5 | 5.1 (5.0 to 5.1) |

- The AMD cell that had a starved run now gives 19.2 to 19.6 GB/s in all five runs. Five runs do not show how often a starved answer occurs, only that none of these five was slow.
- A 3 MiB batch is the best measured on both instances, 4% above 4 MiB. The benchmark's default is now 3 MiB.
- 69 of the 173,867 uploads in the Graviton4 PUT rows here needed a second attempt. The AMD instance was running another client against the same bucket at the time.

## Several clients side by side

Each process is a complete client pinned to its own CPU, sharing nothing with the others, with 128 connections. Runs of 10 s. HTTPS PUT used a 4 MiB batch in the first run of each row and 3 MiB in the others. The rows with one run are single measurements.

### Graviton4

| Request | Processes | Runs | GB/s in total | Whole machine, cores busy |
|---|---|---|---|---|
| HTTPS PUT | 1 | 1 | 5.0 | 1.2 |
| HTTPS PUT | 2 | 1 | 9.8 | 2.7 |
| HTTPS PUT | 4 | 3 | 18.9 to 20.6 | 6.0 to 7.7 |
| HTTPS PUT | 8 | 3 | 34.5 to 36.2 | 13.5 to 14.0 |
| GET | 2 | 1 | 8.3 | 4.0 |
| GET | 4 | 3 | 15.3 to 15.4 | 5.3 to 6.9 |
| GET | 8 | 3 | 26.1 to 26.4 | 10.7 to 10.9 |
| HTTPS GET | 2 | 1 | 5.2 | 3.1 |
| HTTPS GET | 4 | 3 | 9.8 to 10.0 | 4.7 to 5.3 |
| HTTPS GET | 8 | 3 | 17.6 to 17.9 | 10.5 to 12.8 |

### AMD

| Request | Processes | Runs | GB/s in total | Whole machine, cores busy |
|---|---|---|---|---|
| HTTPS PUT | 1 | 1 | 7.0 | 1.4 |
| HTTPS PUT | 2 | 1 | 14.6 | 3.4 |
| HTTPS PUT | 3 | 2 | 21.8 to 22.1 | 5.9 to 6.3 |
| HTTPS PUT | 4 | 3 | 24.0 to 24.0 | 6.4 to 7.6 |
| HTTPS PUT | 8 | 1 | 16.7 | 16.7 |
| GET | 2 | 1 | 14.3 | 4.7 |
| GET | 4 | 3 | 22.0 to 22.1 | 5.1 to 5.2 |
| GET | 8 | 3 | 24.8 to 26.1 | 8.4 to 8.9 |
| HTTPS GET | 2 | 1 | 9.2 | 2.7 |
| HTTPS GET | 4 | 3 | 14.7 to 14.8 | 4.8 to 5.2 |
| HTTPS GET | 8 | 3 | 21.6 to 22.1 | 10.1 to 12.3 |

- Throughput grows with the number of clients until the card is full. Eight clients upload over HTTPS at 34.5 to 36.2 GB/s on Graviton4, which is the card, against 5.0 for one.
- Each client costs 1.2 to 1.9 cores of the machine, counting kernel network work on other cores.
- On the AMD instance four clients fill the 200 Gbit/s card over HTTPS at 24.0 GB/s. Eight gave 16.7 GB/s in one run: more than the card can carry made it slower. Three give 21.8 to 22.1.
- These are separate processes. A client that runs several I/O threads itself is planned and not built.

## Other clients on the same machines

s5cmd 2.3.0 and the AWS CLI 2.40.2 with its CRT transfer client (`preferred_transfer_client crt`, `target_bandwidth 300Gb/s`), run after the passes above from the same instances against the same bucket. Each run sends or fetches 8,000 objects of 8 MiB (67 GB). The source is 8,000 names for one file in a RAM file system, and downloads go to the same RAM file system, so no disk is involved. The multipart threshold was set above 8 MiB so every object is one request.

### Graviton4

| Client and request | Runs | GB/s | Whole machine, cores busy | Seconds per run |
|---|---|---|---|---|
| s5cmd, 256 workers, PUT over HTTP | 3 | 13.8 to 14.4 | 47.8 to 49.0 | 4.7 to 4.9 |
| s5cmd, 1,024 workers, PUT over HTTP | 3 | 20.4 to 22.1 | 69.4 to 72.0 | 3.0 to 3.3 |
| s5cmd, 256 workers, PUT over HTTPS | 3 | 11.8 to 14.3 | 44.4 to 54.0 | 4.7 to 5.7 |
| AWS CLI, PUT over HTTP | 3 | 6.4 to 6.5 | 5.3 to 5.9 | 10.3 to 10.5 |
| AWS CLI, PUT over HTTPS | 3 | 6.1 to 6.5 | 7.1 to 8.8 | 10.2 to 11.1 |
| AWS CLI, PUT over HTTP, checksums only when required | 3 | 6.3 to 6.5 | 4.7 to 5.1 | 10.4 to 10.6 |
| s5cmd, 256 workers, GET over HTTP | 2 | 8.2 to 11.0 | 27.5 to 42.4 | 6.1 to 8.2 |
| s5cmd, 256 workers, GET over HTTPS | 2 | 11.2 to 11.5 | 42.6 to 43.0 | 5.8 to 6.0 |
| AWS CLI, GET over HTTP | 2 | 3.9 to 4.0 | 5.6 to 5.8 | 16.9 to 17.2 |
| AWS CLI, GET over HTTPS | 2 | 4.0 to 4.0 | 6.2 to 6.5 | 16.7 to 16.9 |

### AMD

| Client and request | Runs | GB/s | Whole machine, cores busy | Seconds per run |
|---|---|---|---|---|
| s5cmd, 256 workers, PUT over HTTP | 3 | 14.1 to 14.7 | 41.7 to 45.1 | 4.6 to 4.8 |
| s5cmd, 1,024 workers, PUT over HTTP | 3 | 14.4 to 20.1 | 39.4 to 51.0 | 3.4 to 4.7 |
| s5cmd, 256 workers, PUT over HTTPS | 3 | 13.3 to 14.3 | 42.7 to 47.5 | 4.7 to 5.0 |
| AWS CLI, PUT over HTTP | 3 | 6.2 to 6.4 | 5.3 to 6.2 | 10.5 to 10.9 |
| AWS CLI, PUT over HTTPS | 3 | 5.6 to 6.6 | 6.7 to 7.6 | 10.2 to 11.9 |
| AWS CLI, PUT over HTTP, checksums only when required | 3 | 6.0 to 6.3 | 5.3 to 6.2 | 10.7 to 11.2 |
| s5cmd, 256 workers, GET over HTTP | 2 | 10.3 to 11.8 | 57.3 to 62.9 | 5.7 to 6.5 |
| s5cmd, 256 workers, GET over HTTPS | 2 | 11.2 to 11.6 | 59.7 to 60.0 | 5.8 to 6.0 |
| AWS CLI, GET over HTTP | 2 | 4.7 to 4.7 | 5.1 to 5.5 | 14.2 to 14.3 |
| AWS CLI, GET over HTTPS | 2 | 4.8 to 5.1 | 5.5 to 5.8 | 13.2 to 13.9 |

Side by side, best figures on the Graviton4 instance:

| Request | s5cmd | AWS CLI with CRT | One `s3_uring` client | Eight `s3_uring` clients |
|---|---|---|---|---|
| PUT over HTTP | 22.1 GB/s on 72 cores | 6.5 on 6 | 36.7 on 3.9 | not run: one fills the card |
| PUT over HTTPS | 14.3 on 54 | 6.5 on 7 | 5.2 on 1.2 | 36.2 on 14 |
| GET over HTTP | 11.0 on 42 | 4.0 on 6 | 4.4 on 1.2 | 26.4 on 11 |
| GET over HTTPS | 11.5 on 43 | 4.0 on 6 | 2.8 on 1.1 | 17.9 on 11 |

- One client thread beats both for plain PUT, and uses far fewer cores per byte than either for every request.
- One client thread is slower in total than s5cmd for HTTPS and for GET, because s5cmd spreads the work over 40 to 60 cores. Four or more clients side by side pass it.

## Linux 6.1

A `c7gn.large` with Amazon Linux 2023 (Linux 6.1.190) ran the self-test over HTTP and HTTPS, the real-kernel check of a measured connect, and PUT runs with 32 connections. With the preference on, 32 of 32 connections in use had large segments (8,228 bytes), against 28 of 32 with it off.

One thing seen there and not explained: five seconds into those runs, the 8 or 16 small-segment connections the client had already closed and replaced were still listed as established. They were gone once the process ended. On Linux 7.0 the same sample showed none. It was not investigated.

## Caveats

- The other clients read files and write files. Even from RAM that is a copy between the page cache and their buffers which `s3_uring` does not make, because it sends and receives in registered memory and keeps nothing. They do more work per byte by design.
- The AWS CLI is one program built on the CRT library. Its figures are the CLI's, and may be below what the library does when used directly.
- The other clients' runs lasted 3 to 17 s and include their start-up. s5cmd's upload runs were 3 to 6 s.
- s5cmd signs the body of a PUT over plain HTTP with SHA-256, and the AWS CLI adds a CRC to uploads by default. Those are their defaults. The CLI was also run with checksums only when required.
- The preference concentrates connections. With it on, the Graviton4 runs used 4 of the 8 addresses in the first sample taken. S3 did not throttle at these rates. A store that limits load per address might.
- The multi-client rows used processes placed on consecutive even-numbered CPUs with no attention to which cores take the card's interrupts.
- Everything here is one afternoon, one region, one bucket.
