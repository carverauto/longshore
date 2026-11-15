# Milestone 25: AES-GCM sealing with the memory pattern of an upload

Measured 2026-11-15 with `cargo bench -p longshore-bench --bench aead_memory` (criterion, 1 s warm-up, 3 s of measurement), pinned to CPU 1 on the AWS instances and to a fast core on the DGX Spark. Three runs per machine. Every figure is the median, in GiB/s (2^30 bytes), of sealing 8 MiB as TLS 1.3 records with AES-128-GCM through the AWS-LC call the client uses. Raw output: `m25-aes-gcm-memory.txt`.

## The question

Milestone 13 measured the cipher at 25.6 GiB/s on the AMD Zen 5 instance, and milestone 20 measured HTTPS upload to S3 from the same instance type at 7.3 GB/s with the client's thread full. The profile in milestone 18 put 55% of that thread inside AES-GCM, which makes the cipher's speed inside the client about 12 GiB/s: half of what the cipher benchmark gave.

The cipher benchmark seals every record into one 16 KiB destination that it reuses, so its output never leaves the processor's cache. The client cannot do that. Each connection seals megabytes into its own send buffer, which has to stay as it is until the network card has taken it, and the bodies it reads are different memory each time.

## Four patterns

- hot: one 8 MiB source, one 16 KiB destination reused. What milestone 13 measured.
- batch: one 8 MiB source sealed into one 8 MiB destination, both reused.
- cold: 32 sources and 32 destinations of 8 MiB taken in turn, 256 MiB of each, so both are out of the cache when they are touched. This is the client's pattern.
- cold, in place: 32 bodies taken in turn, each sealed over itself, with only the 17 bytes that follow a record written elsewhere.

| Processor | Machine | hot | batch | cold | cold, in place | cold as a share of hot | in place over cold |
|---|---|---|---|---|---|---|---|
| AMD EPYC 9R05, Zen 5 | AWS `m8azn.24xlarge` | 25.7 | 24.3 | 13.8 | 21.5 | 54% | 1.56 |
| AMD EPYC 9R05, Zen 5 | AWS `m8azn.large` | 25.6 | 23.8 | 13.6 | 20.7 | 53% | 1.52 |
| AMD EPYC 9R45, Zen 5 | AWS `m8a.large` | 22.8 | 21.1 | 12.9 | 18.9 | 57% | 1.47 |
| AMD EPYC 9R14, Zen 4 | AWS `c7a.large` | 10.5 | 10.3 | 8.8 | 10.4 | 84% | 1.18 |
| Intel Xeon 6975P-C, Granite Rapids | AWS `c8i.large` | 16.2 | 13.1 | 7.1 | 8.9 | 44% | 1.25 |
| Intel Xeon 8488C, Sapphire Rapids | AWS `c7i.large` | 12.8 | 10.8 | 5.0 | 7.1 | 39% | 1.42 |
| Graviton4, Neoverse V2 | AWS `c8gn.24xlarge` | 6.7 | 6.7 | 6.4 | 6.5 | 96% | 1.02 |
| Graviton4, Neoverse V2 | AWS `c8g.large` | 6.2 | 6.0 | 5.9 | 5.9 | 95% | 1.00 |
| Arm Cortex-X925, default build | DGX Spark | 7.7 | 7.6 | 7.4 | 7.4 | 96% | 1.00 |

## What the figures say

- On the fast x86 processors the cipher in the client's memory pattern runs at 39 to 57% of its speed with the output in cache. On Zen 5 that is 13.8 GiB/s against 25.7. The cipher is not slow there. It is waiting for memory.
- That accounts for the gap the question starts from. The profile's 55% of a thread at 7.1 GB/s is 12 GiB/s for the cipher inside the client, and the cold pattern measures 13.8. On Graviton4 the profile in milestone 18 gives 72% of a thread at 4.9 GB/s, which is 6.3 GiB/s, and the cold pattern measures 6.4.
- On the Arm processors the pattern makes no difference, because the cipher there is slower than the memory under it.
- Sealing in place is 1.2 to 1.6 times faster than sealing to a separate destination on x86, and no faster on Arm. A separate destination costs three streams of memory traffic for each byte: the source is read, the destination is read so that it can be written, and it is written. In place there are two.
- The `.large` and the `24xlarge` instance of the Zen 5 `m8azn` agree to within 4% in every column. The two Graviton4 rows are different instance families and differ by up to 8%.

## What this does not show

- Throughput through the client. Nothing here was sent anywhere. What in-place sealing would be worth for an HTTPS upload has not been measured: on Zen 5 the cipher is about 55% of the client's thread, so a cipher 1.56 times faster would free about a fifth of the thread, if nothing else changed.
- That in-place sealing fits the client. It replaces the caller's bytes with ciphertext, so a retry on another connection would need the plaintext back, and each record's header and trailer would have to be sent from other memory, as a send of many pieces.
- Opening records on a download, which has the same shape and was not measured.
- The Sapphire Rapids instance read 12.8 GiB/s hot where milestone 13 read 16.2 on the same instance type. One instance each time. The cause was not looked for.
- Memory other than what a single thread can pull. With several I/O threads sealing at once they share the machine's memory bandwidth, and the cold figures would be lower.
