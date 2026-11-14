# Milestone 21: object sizes from 1 to 64 MiB

Measured 2026-11-13 with `s3_uring` against a general-purpose S3 bucket in us-east-1, from a `c8gn.24xlarge` instance in the same region (Graviton4, 300 Gbit/s card, the image used in milestone 20). One client thread pinned to CPU 2, 128 connections, the preference for large-segment addresses on (`S3_PREFER_LARGE_SEGMENTS=2`). Five runs of 8 s per cell. GB = 10^9 bytes. Raw output: `m21-object-sizes.jsonl`, where the label of each line gives the size in MiB.

The question was how large an object has to be before the fixed cost of a request stops mattering. The connection count is held at 128 for every size, so throughput here is requests in flight times bytes per request, and a small object shows the per-request cost directly.

| Object | PUT, GB/s median (lowest to highest) | Client thread | GET | Client thread | HTTPS PUT | Client thread |
|---|---|---|---|---|---|---|
| 1 MiB | 2.92 (2.87 to 2.95) | 0.23 | 3.42 (2.87 to 3.64) | 0.81 | 2.39 (2.32 to 2.40) | 0.63 |
| 2 MiB | 5.36 (5.02 to 5.40) | 0.29 | 4.53 (3.96 to 4.64) | 0.96 | 3.92 (3.91 to 4.08) | 0.99 |
| 4 MiB | 8.01 (7.89 to 8.05) | 0.27 | 4.77 (4.72 to 4.91) | 0.99 | 4.86 (3.79 to 4.97) | 0.99 |
| 8 MiB | 10.46 (10.26 to 10.49) | 0.29 | 4.82 (4.78 to 4.94) | 0.99 | 4.94 (4.89 to 5.05) | 0.99 |
| 16 MiB | 12.14 (12.04 to 12.20) | 0.29 | 4.86 (4.85 to 5.04) | 1.00 | 5.02 (4.88 to 5.09) | 1.00 |
| 32 MiB | 13.20 (13.07 to 13.25) | 0.25 | 4.88 (4.77 to 5.00) | 1.00 | 4.85 (4.81 to 4.92) | 1.00 |
| 64 MiB | 13.91 (13.84 to 13.96) | 0.25 | 4.63 (4.59 to 4.73) | 1.00 | 4.30 (4.28 to 4.31) | 1.00 |

## What the figures say

- Plain PUT is never bound by the client here. Its thread is a quarter busy at every size. With 128 requests in flight, the time one request takes fits 32 ms plus 9.1 ms per MiB. The 32 ms is spent waiting for S3, and it is 78% of a 1 MiB request, 64% of 2 MiB, 47% of 4 MiB, 31% of 8 MiB, 18% of 16 MiB, 10% of 32 MiB and 5% of 64 MiB.
- That fixed time costs throughput per connection and no client CPU. More connections buy the throughput back: milestone 20 reaches 36.7 GB/s with 8 MiB objects on 640 connections.
- GET and HTTPS PUT are bound by the client's core from 2 to 4 MiB up, at about 4.8 and 4.9 GB/s. Below that the per-request wait is the limit, as for plain PUT.
- At 64 MiB, GET and HTTPS PUT are slower than at 32 MiB by 5% and 11%. The cause was not looked for.

## What this does not show

- One instance type and one run of the sweep. The AMD instance was not measured.
