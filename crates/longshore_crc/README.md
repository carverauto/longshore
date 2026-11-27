# longshore_crc

[![crates.io](https://img.shields.io/crates/v/longshore_crc.svg)](https://crates.io/crates/longshore_crc)
[![docs.rs](https://img.shields.io/docsrs/longshore_crc)](https://docs.rs/longshore_crc)
[![License](https://img.shields.io/crates/l/longshore_crc.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

The checksums object stores verify: CRC-64/NVME for Amazon S3 and Azure,
and CRC-32C for Google Cloud Storage. Neither allocates, and the crate
depends on nothing outside the standard library.

At run time the crate picks the fastest kernel the processor supports:
carry-less multiplication on x86-64 (`VPCLMULQDQ` with AVX-512 or AVX2,
else `PCLMULQDQ`) and on AArch64 (`PMULL`), and a lookup table elsewhere.

## Example

```rust
use longshore_crc::{crc32c, crc64_nvme, Crc64Nvme};

assert_eq!(crc64_nvme(b"123456789"), 0xae8b14860a799888);
assert_eq!(crc32c(b"123456789"), 0xe3069283);

// A body that arrives in pieces gives the same checksum.
let mut crc = Crc64Nvme::new();
crc.update(b"12345");
crc.update(b"6789");
assert_eq!(crc.finish(), 0xae8b14860a799888);
```

## What it gives

- `crc64_nvme` and `crc32c` over one slice, and `Crc64Nvme` and `Crc32c`
  over a body in pieces.
- `crc64_header_value`, `crc64_azure_header_value` and
  `crc32c_header_value`: the checksum as the base64 text a request header
  carries, in a fixed array.
- `base64`, which encodes into a buffer the caller provides.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::checksum`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead. The crates are
released together: depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_crc>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
