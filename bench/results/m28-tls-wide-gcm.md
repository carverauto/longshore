# Milestone 28: why HTTPS costs more than HTTP, and the 8-way AES-GCM on Apple and Cortex-X925 cores under Linux

Measured 2026-11-23 with `http_uring put|get` against `http_uring sink` over loopback, in a Linux container (Linux 7.2, OrbStack) on an Apple M3 Max: 4 vCPUs, the sink on CPUs 0 to 2 and the client pinned to CPU 3. 8 connections, 8 MiB bodies, 5 s per run, three runs per cell, medians. GB = 10^9 bytes. CPU per GB is the client thread's. Profiles with `perf record -F 2000` over the client's run.

## Where the time goes

Before the change, at commit `f1a40c6`:

| | GB/s | Client CPU per GB | Kernel entries per request |
|---|---|---|---|
| PUT | 9.90 | 101 ms | 0.5 |
| PUT, TLS | 5.01 | 200 ms | 3.0 |
| GET | 3.86 | 259 ms | 1.0 |
| GET, TLS | 2.46 | 405 ms | 1.2 |

The client's profile under TLS:

| | AES-GCM (AWS-LC assembler) | Kernel | Everything else |
|---|---|---|---|
| PUT, TLS | 65.5% `aes_gcm_enc_kernel` | 27.7%, the send path (memcpy 9.7%) | under 7%, longshore's own code under 0.5% per function |
| GET, TLS | 35.1% `aes_gcm_dec_kernel` | 58.7% `copy_to_user`, the receive copy plain GET also pays | under 3% |

So HTTPS costs what the cipher costs. On upload the cipher is two thirds of the core; on download TLS adds the decryption and nothing else. None of longshore's own code shows up. The record layer already seals from the caller's buffer and opens into it (`EVP_AEAD_CTX_seal_scatter`, `open_gather`), so the body is not copied for TLS.

`aes_gcm_enc_kernel` and `aes_gcm_dec_kernel` are AWS-LC's 4-way routines. AWS-LC has an 8-way pair (`aesv8_gcm_8x_*`) that also uses the SHA-3 `EOR3` instruction, but takes it only on CPUs it recognises: Neoverse V1, V2 and V3 by their part number, and Apple M cores only under macOS. This CPU reports `aes pmull sha3` and implementer 0x61 (Apple), yet under Linux it got the 4-way routines, as the DGX Spark's Cortex-X925 did in milestone 11.

## The change

`longshore_tls::wide_gcm::enable`, called before the first key is set, reads the core's Main ID Register as AWS-LC does and lists two kinds of core: any Apple core (implementer 0x61), and Arm's Cortex-X925 (part 0xD85). On those, when AWS-LC has found AES, PMULL and SHA3, it sets the capability bit AWS-LC sets for Apple M cores. Apart from the AES-GCM choice that bit only selects between two big-number multiplication routines that run on any AArch64 CPU, so it never selects code the CPU cannot run. Every other core keeps AWS-LC's own choice. Graviton needs nothing: Graviton 3 and 4 are Neoverse V1 and V2, which AWS-LC recognises, and Graviton 2 (Neoverse N1) lacks SHA3. Unlike `scripts/build-fast-aes-aarch64.sh`, the binary stays safe on any AArch64 CPU.

Whether the routine is in use (`wide_gcm::active`, test `tls_wide_gcm`):

| | Main ID Register | AWS-LC by itself | With the change |
|---|---|---|---|
| macOS, Apple M3 Max | not read | 8-way (brand "Apple M") | 8-way, unchanged |
| Linux guest on the same Mac | `0x610f0000` (Apple; the part number is hidden) | 4-way | 8-way |

## After

`main` (`f1a40c6`) and the change, run back to back in the same session, three runs each:

| | `main` | With the change | Change |
|---|---|---|---|
| PUT, TLS | 5.97 (5.93 to 5.97) | 7.02 (6.64 to 7.31) | +18% |
| GET, TLS | 2.71 (2.70 to 2.71) | 2.86 (2.85 to 2.90) | +6% |
| GET | 4.16 | 4.08 | noise |

Plain PUT ranged from 10.6 to 13.9 GB/s in these runs with no change to its code, and the whole session ran faster than the one that measured the tables above; compare only runs from the same session. In an earlier session the change measured +14% upload and +11% download, and the profile showed `aesv8_gcm_8x_enc_128` at 55.5% of the client on upload and `aesv8_gcm_8x_dec_128` at 28.2% on download.

## What this does not tell us

- x86-64: unchanged; AWS-LC picks its fastest routine there by itself.
- Cores off the list with the instructions (Cortex-A725, Neoverse N2 and others): the 8-way routine is unmeasured there, so they keep the 4-way routine.
- The DGX Spark has ten Cortex-X925 and ten Cortex-A725 cores. The register is read once, on whichever core runs the first key setup; if that is an X925 the routine is used on every core, A725 included, where it runs correctly at a speed not yet measured.
- A real store over a real network. Milestone 11 measured the 8-way routine on the Cortex-X925 at 12.2 against 7.8 GiB/s for sealing and 7.5 against 5.9 GB/s for an HTTPS upload; with this change that core gets it without a special build.
- What is left: the cipher is still the larger part of an HTTPS upload. Sealing in place (milestone 25) would help on x86 only; kernel TLS (milestone 12) would take a third off at most.
