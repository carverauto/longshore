# Milestone 6 — AES-GCM candidates for the TLS 1.3 record layer

Measured 2026-11-12 with `cargo bench -p longshore-bench --bench aead` (criterion), one core.

Each candidate seals 512 records of 16,384 bytes (8 MiB of application data): AES-128-GCM over the data plus the one-byte content type TLS 1.3 appends, reading from a source buffer and leaving ciphertext and tag in a destination buffer. Libraries that can only encrypt in place are charged for the copy from source to destination, because the record layer would have to make it. Before timing, all four candidates had to produce byte-identical records for the same key, nonce and input.

References from the talk: 2.2 GB/s (Zig standard library), 7.4 GB/s (their optimized version), about 12% gained by passing the suffix separately.

## Results, GB/s (10^9 bytes)

| Candidate | arm64 Cortex-X925, pinned | x86-64 Xeon 8358P (Ice Lake), shared | x86-64 Xeon 8176 (Skylake), shared | Apple M-series |
|---|---|---|---|---|
| `aes-gcm` (RustCrypto), copy + in-place | 4.6 | 2.6 | 1.9 | 6.6 |
| `ring`, copy + in-place | 7.5 | 5.8 | 4.1 | 7.2 |
| `aws-lc-rs`, copy + in-place | 7.5 | 8.4 | 4.3 | 9.3 |
| AWS-LC `EVP_AEAD_CTX_seal_scatter`, no copy | **8.3** | **11.2** | **5.2** | **10.9** |

## What it shows

- **The no-copy interface wins everywhere.** Against the same library with a copy it is 11% faster on the X925, 34% on Ice Lake, 21% on Skylake and 17% on the Mac. The talk reported about 12%.
- **AWS-LC's scatter call is exactly the interface the talk describes**: separate input and output buffers plus a separate suffix. It is in AWS-LC's C API; the safe `aws-lc-rs` wrapper does not expose it, so using it means calling `aws-lc-sys` directly.
- **Against the 7.4 GB/s reference:** the scatter path clears it on the X925 (8.3) and Ice Lake (11.2). On the 2017 Skylake Xeon nothing clears it; the best is 5.2. `ring`, which the host project already uses, clears it only on arm64 and only barely.
- **The pure-Rust `aes-gcm` crate plays the role the Zig standard library played in the talk**: 1.9 to 4.6 GB/s on the Linux hosts.

## Recommendation

Use AWS-LC through the scatter call for record sealing. It is the only candidate that meets the throughput reference on both architectures and the only one that needs no copy of the payload.

Cost: a second cryptographic library beside `ring`, called through an unsafe FFI function, with a C and assembly build. If that is unwelcome, `ring` with a copy is the fallback at 7.5 GB/s on arm64 and 5.8 on Ice Lake.

## Caveats

- Both Xeon hosts were running VMs and the benchmark was not pinned there.
- AES-128-GCM only; AES-256-GCM, which TLS 1.3 also offers, was not measured.
- Opening (decrypting) records was not measured.
- A single destination record was reused, so the destination was always warm in cache; a real send path cycles through larger registered buffers.
