# Milestone 13 — AES-GCM sealing speed by CPU

Measured 2026-11-13 with `cargo bench -p osc-bench --bench aead` (criterion), the `aws-lc scatter, no copy` case: sealing 8 MiB as TLS 1.3 records with AES-128-GCM through the same AWS-LC calls the client uses. One core, GiB/s (2^30 bytes), 3 s of measurement. The AWS figures are from the smallest instance of each family, launched for the test and terminated.

HTTPS upload is bound by this: in the profile of milestone 11 two thirds of the client's core is inside the cipher.

| CPU | Machine | GiB/s |
|---|---|---|
| AMD EPYC 9R05 (Zen 5, 5.0 GHz) | AWS `m8azn` | **25.6** |
| AMD EPYC 9R45 (Zen 5) | AWS `c8a` | 22.2 |
| Intel Xeon 6975P-C (Granite Rapids) | AWS `c8i` | 16.2 |
| Intel Xeon 6975P-C (Granite Rapids) | AWS `c8in` | 16.0 |
| Intel Xeon Platinum 8488C (Sapphire Rapids) | AWS `c7i` | 16.2 |
| Arm Cortex-X925, 8-way routine forced at build time | DGX Spark | 12.2 |
| AMD EPYC 9R14 (Zen 4) | AWS `c7a` | 10.4 |
| Arm Cortex-X925, default build | DGX Spark | 7.8 |
| AWS Graviton4 (Neoverse V2), 8-way routine chosen automatically | AWS `c8g` | 6.1 |

For reference, the talk gives 7.4 GB/s (6.9 GiB/s) for its optimised AES-GCM.

## Reading it

- **The newest AMD cores are the fastest by a wide margin**, about 3.7 times the talk's cipher figure, with a stock build: on x86-64 AWS-LC detects the wide vector AES instructions by itself.
- **Graviton4 is the slowest**, half the DGX's core, even though AWS-LC picks its faster routine there. Arm is where our checksum and plain-upload numbers are best; for TLS it is the wrong choice.
- **On the DGX the default build leaves a third on the table**, because AWS-LC does not recognise the Cortex-X925; see milestone 11.
- **`m8azn` pairs the fastest cipher with 200 Gbit/s networking**, so it is the instance to measure HTTPS upload on. That run has not been done: the account's vCPU quota does not yet allow two of the large sizes.

## Caveats

- A microbenchmark of the cipher alone. End-to-end HTTPS will be lower; on the DGX the full path ran at about 60% of the cipher's own speed.
- Small shared instances, one run each.
