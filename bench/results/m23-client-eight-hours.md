# Milestone 23: the shareable client against S3 for eight hours, and multipart uploads through it

Measured 2026-11-14 against a general-purpose S3 bucket in us-east-1, from one `c8gn.4xlarge` instance (Graviton4, 16 vCPUs) on Linux 7.0.0-1014-aws. 8 MiB objects and parts, GB = 10^9 bytes, HTTPS unless stated. Raw output: `m23-client-eight-hours.jsonl`, `m23-soak-progress.txt` (one sample a minute) and `m23-soak-faults.txt` (the time of each injected failure).

## Eight hours with failures injected

`client_s3 mix` ran for 28,800 s: four I/O threads with 64 connections each, four caller threads, the first thread on CPU 2 and the rest two CPUs apart. It fetches as fast as it can and stores 100 objects a second. Every object holds a pattern that names it, every fetch checks bytes of that pattern, and every store carries a CRC-64. While it ran, a script on the same machine:

- every 40 s closed every TCP connection to port 443 with `ss -K` (698 times),
- after every fourth of those also dropped all outgoing packets to port 443 for 5 s with an `iptables` rule (174 times),
- every 120 s rewrote the client's credentials file from the instance role.

That `ss -K` aborts an open connection to S3 was checked on the same machine before the run started: the next use of a connection it had closed failed (`ConnectionAbortedError`).

| | |
|---|---|
| Requests | 23,231,394 |
| Objects fetched | 20,351,688 |
| Fetches whose bytes were not the object's pattern | 0 |
| Objects stored | 2,881,080 |
| Operations that returned an error to the caller | 0 |
| Throughput, whole run | 6.77 GB/s on 3.14 cores |
| Throughput per 60 s sample | 6.30 to 7.02 GB/s, median 6.99 |
| Resident memory, sampled every 60 s | 6,151 MB at 467 of 480 samples, 6,152 to 6,154 MB at the other 13. The last sample is 6,151 |
| Open file descriptors, sampled every 60 s | 16 to 19 at 474 of 480 samples. Six samples read 79 to 122, and the sample after each read 16 |
| Credentials adopted from the file | 32 across the four I/O threads. No reload failed |

Five of the six high descriptor samples are 660 s apart, in the first hour. The cause was not looked for.

## Multipart uploads through the client

`client_s3 multipart` uploads objects through `upload_begin`, `upload_part` and `upload_complete` with all the parts of an object in flight together, reads every part back with a ranged fetch and compares it, and then checks that completing an aborted upload is refused. Eight connections per I/O thread.

| Transport | I/O threads | Parts per object | Objects | Parts read back and correct | Completing an aborted upload |
|---|---|---|---|---|---|
| HTTP | 1 | 4 | 4 | 16 of 16 | refused |
| HTTP | 2 | 8 | 4 | 32 of 32 | refused |
| HTTPS | 2 | 8 | 4 | 32 of 32 | refused |
| HTTPS | 4 | 16 | 3 | 48 of 48 | refused |

These ran on the same instance while the eight hour run was using it, so their times in the raw file are not measurements of speed.

## What this does not show

- Eight hours on one instance type and one kernel, not days.
- The run breaks connections. It does not make an I/O thread or its ring fail. That is tested by `client_faults` on Linux 6.8 and 6.14, not against a store.
- Requests that failed and were retried inside the client are not counted. Only what the caller saw is.
- The multipart runs are a check of correctness against one store. Parts were 8 MiB, objects at most 128 MiB, and no failure was injected during them.
- One caller thread per I/O thread.
