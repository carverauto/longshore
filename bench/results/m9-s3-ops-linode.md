# Milestone 9 — Request rates for the other S3 operations, Linode

Measured 2026-11-12 with `s3_uring` from host A (DGX Spark, AArch64, Linux 6.14) over the internet to Linode Object Storage in `us-ord` (ping 8.7 ms), one io_uring thread, 16 connections, 8 s per run, one run per cell. Raw output: `m9-s3-ops-linode.jsonl`.

These are the operations the throughput benchmarks did not cover: a conditional store of a new 4 KiB object, a ranged fetch of the first 4 KiB of a 1 MiB object, an erase, a listing of 200 keys, and whole-object PUT and GET of 4 KiB objects.

| Operation | Protocol | Requests/s | Retried | Median below | 99% below | Client CPU per request |
|---|---|---|---|---|---|---|
| Conditional store, new 4 KiB key | HTTP | 310 | 0% | 66 ms | 131 ms | 61 µs |
| | HTTPS | 299 | 0% | 66 ms | 131 ms | 102 µs |
| Ranged fetch, 4 KiB of 1 MiB | HTTP | 233 | 0% | 131 ms | 131 ms | 86 µs |
| | HTTPS | 229 | 0% | 131 ms | 131 ms | 94 µs |
| Erase | HTTP | 219 | 12% | 33 ms | 1.05 s | 80 µs |
| | HTTPS | 204 | 14% | 33 ms | 1.05 s | 84 µs |
| List 200 keys (49 kB reply) | HTTP | 98 | 4% | 262 ms | 524 ms | 154 µs |
| | HTTPS | 96 | 2% | 262 ms | 524 ms | 153 µs |
| PUT, 4 KiB | HTTP | 252 | 0% | - | - | 76 µs |
| | HTTPS | 258 | 0% | - | - | 81 µs |
| GET, 4 KiB | HTTP | 238 | 0% | - | - | 101 µs |
| | HTTPS | 239 | 0% | - | - | 117 µs |

Latencies come from a histogram with power-of-two buckets, so "below 131 ms" means between 66 and 131 ms.

## Reading it

- **Every rate here is the store's, not the client's.** Each request takes the store 50 to 250 ms, against a network round trip under 9 ms, and the client thread was 1.5% to 3% busy throughout. Sixteen connections each waiting on the store give 100 to 300 requests per second, whatever the client can do.
- **The client's cost per request is 60 to 150 µs of CPU**, which would be 7,000 to 16,000 requests per second on one core if nothing else were the limit. That is an estimate from CPU time, not a measured rate, and it is pessimistic: at these low rates the periodic timer ticks that fire whether or not there is work are a visible share of it.
- **Erase and list were throttled.** 12 to 14% of erases and 2 to 4% of listings needed a second attempt, and the slowest erases took about a second, which is the client's backoff after a slow-down answer. The client retried them internally and every operation succeeded.
- **TLS makes no difference at this scale.** The store's latency hides it; the CPU cost of TLS on small requests is within the noise of these runs.
- **A listing costs the client the most CPU**, about 150 µs, because the reply is 49 kB and is decoded from chunked transfer encoding.

## What this does not tell us

- The client's real ceiling for small requests. That needs a store that answers in well under a millisecond: an in-region instance or a store on the local network.
- Anything about x86-64.
- Variance: single 8-second runs.
