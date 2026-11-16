# longshore_crc

The checksums object stores verify: CRC-64/NVME, which Amazon S3 uses, and
CRC-32C, which Google Cloud Storage uses. Neither allocates.

Both are computed in this crate. When the program runs, the fastest kernel
the processor supports is chosen: carry-less multiplication on x86-64
(`VPCLMULQDQ` with AVX-512 or AVX2, else `PCLMULQDQ`) and on AArch64
(`PMULL`), and a lookup table on anything else. It uses nothing outside the
standard library.

```rust
let crc = longshore_crc::crc64_nvme(b"123456789");
assert_eq!(crc, 0xae8b14860a799888);
```

## Part of longshore

This crate is one of the pieces of [longshore](https://crates.io/crates/longshore),
an object storage client for Linux `io_uring`. Most programs want that crate,
which gathers these under one name. The crates are released together, so
depend on them at the same version.

- Documentation: <https://docs.rs/longshore_crc>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0.
