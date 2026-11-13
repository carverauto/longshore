# Milestone 11 — HTTPS with AWS-LC's 8-way AES-GCM on the DGX Spark

Measured 2026-11-13 with `http_uring` between host A (client) and host B (TLS sink), DGX Spark, AArch64, Linux 6.14, one link. One client io_uring thread pinned to CPU 19 (Cortex-X925), 8 MiB objects, TLS 1.3 with AES-128-GCM, 8 s per run, one run per cell. GB = 10^9 bytes, GiB = 2^30. Raw output: `m11-tls-fast-aes-aarch64-dgx-spark.jsonl`.

## What changed

Nothing in our code. AWS-LC has two AES-GCM routines for AArch64: a 4-way one, and an 8-way one that uses the SHA3 `EOR3` instruction. It picks the 8-way routine only when it recognises the CPU as a Neoverse V1, V2 or V3 or an Apple M core. The DGX Spark's Cortex-X925 has the instructions but is not on that list, so it was getting the 4-way routine.

AWS-LC refuses a run-time override that claims a CPU it did not detect. It does allow its capabilities to be fixed when it is compiled. The "8-way" build here sets, for the C compiler only:

```
CFLAGS_aarch64_unknown_linux_musl="-DOPENSSL_STATIC_ARMCAP -DOPENSSL_STATIC_ARMCAP_NEON \
  -DOPENSSL_STATIC_ARMCAP_AES -DOPENSSL_STATIC_ARMCAP_PMULL -DOPENSSL_STATIC_ARMCAP_SHA1 \
  -DOPENSSL_STATIC_ARMCAP_SHA256 -DOPENSSL_STATIC_ARMCAP_SHA512 -DOPENSSL_STATIC_ARMCAP_SHA3 \
  -DOPENSSL_STATIC_ARMCAP_NEOVERSE_V3"
```

A binary built this way runs only on CPUs that have all of those instructions; on anything else it would crash. It is a build for known hardware, not a default.

## Results

| | Default build | 8-way build |
|---|---|---|
| AES-128-GCM record sealing alone (criterion, `aws-lc scatter`) | 7.78 GiB/s | **12.24 GiB/s** |
| HTTPS PUT, 32 connections | 5.83 GB/s | 6.84 GB/s (client thread 84% busy: peer-limited) |
| HTTPS PUT, 48 connections | 5.87 GB/s | **7.50 GB/s** |
| HTTPS PUT with CRC-64/NVME, 32 connections | 4.97 GB/s | 6.24 GB/s |
| HTTPS PUT with CRC-64/NVME, 48 connections | 4.93 GB/s | 5.97 GB/s |
| HTTPS GET, 16 connections | 3.48 GB/s | 4.03 GB/s |

The client core was full in every run except the one marked.

## Where the time goes now

A profile of the client's core during the 48-connection PUT at 7.5 GB/s (perf, 2 kHz, 6 s):

| Share | Where |
|---|---|
| 66% | The 8-way AES-GCM routine itself |
| 3% | The rest of the cipher (tag, per-record setup) |
| about 6% | The host's packet filter and connection tracking (`nft_do_chain`, `nf_conntrack`): these machines run Kubernetes |
| about 24% | The rest of the kernel's send path and the network driver |
| under 1% | Our own code |

## Reading it

- **HTTPS PUT is now 7.5 GB/s on one core**, 28% up, and level with the 7.4 GB/s the talk gives for its cipher alone. The talk gives no end-to-end TLS figure.
- **It is cipher-bound.** Two thirds of the core is inside the AES-GCM assembler. At 7.5 GB/s with 69% of the core on the cipher, the cipher is running at about 11 GB/s in the real path against 13.1 GB/s (12.24 GiB/s) in isolation; the difference is the plaintext arriving cold from 8 MiB buffers.
- **Our own code is not where the time is.** There is no remaining software overhead of ours to remove; the earlier estimate of 50 ms per GB "around the cipher" was the kernel's send path, which a plain PUT pays too.
- **What could still move it:** a faster cipher core (untested: Graviton4, where AWS-LC selects the 8-way routine by itself); turning off the host packet filter for the test link, worth about 6%; or letting the network card encrypt. This card reports `tls-hw-tx-offload: on`, so kernel TLS with hardware offload is possible in principle. That would replace the in-tree record layer on the send side and is a design decision, not a tuning step.

## Caveats

- One 8-second run per cell.
- The peer is a rustls sink that discards data, built the same way as the client.
- Nothing here has been run on x86-64 or on Graviton.
