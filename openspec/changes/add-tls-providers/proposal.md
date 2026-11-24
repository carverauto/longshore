# Change: TLS through AWS-LC, ring or rustls, chosen by feature, or no TLS at all

## Why
TLS records are sealed and opened with AWS-LC only, and the handshake uses
rustls's AWS-LC provider (issue #58). Some users want `ring`, which longshore
already depends on for request signing, as their only cryptographic
library; others, on internal networks, want no TLS library at all. The
choice belongs to the user of the crate, not to longshore.

stdx/crypto was considered and left out: it is published only through git,
and `cargo publish` refuses git dependencies, so no crate on crates.io can
offer it. It was also slower than AWS-LC for AES-128-GCM on 16 KiB records
(M3 Max, macOS: 6.4 against 10.7 GB/s sealing; Linux guest: 5.9 against 8.0).

## What Changes
- Three provider features on `longshore`, `longshore_s3`,
  `longshore_http_client`, `longshore_stream` and `longshore_tls`:
  - `aws-lc` (default): longshore's record layer with AWS-LC, no copy;
    rustls's AWS-LC provider for the handshake.
  - `ring`: longshore's record layer with ring, one copy per record;
    rustls's ring provider for the handshake.
  - `rustls`: rustls for everything, its record encryption included, with
    its ring provider. rustls allocates a buffer for every record it seals
    and copies twice; that allocation runs inside the same exception the
    handshake uses. For users who want all of TLS from rustls.
  `tls` stays as the name for "TLS with the default provider" and turns on
  `aws-lc`.
- With neither feature the crates build without `longshore_tls`, `rustls`,
  `aws-lc-sys` and `aws-lc-rs`, as `--no-default-features` does today.
  `ring` stays in that build for SigV4 signing.
- With several features, the client chooses per build:
  `Builder::tls_provider(TlsProvider::Ring)`; the default is the first
  compiled in of AWS-LC, ring and rustls. Asking for a provider that is not compiled
  in fails `build()` with `BuildError::Setting` naming it.
- `longshore_tls::record::Direction` holds an AWS-LC context (sealing from
  the caller's buffer, no copy, as now), a ring key (copying the body into
  the record buffer and sealing in place) or rustls's encrypter and
  decrypter. Records are byte-identical across the three; AWS-LC and ring
  allocate nothing per record.
- **BREAKING** (`longshore_tls` only): `Direction::open` takes the record as
  `&mut [u8]`, since rustls decrypts where the record lies.
- The 8-way AES-GCM selection (`wide_gcm`) applies to the AWS-LC provider only.
- CI lints and tests the ring-only build and checks its dependency tree
  holds no AWS-LC crate.

## Impact
- Affected specs: `server-networking` (MODIFIED: TLS as a build feature),
  `tls-providers` (ADDED)
- Affected code: `longshore_tls` (`record.rs`, `handshake.rs`, `wide_gcm.rs`,
  `Cargo.toml`), the feature lists of the four crates above,
  `longshore_s3` client `Builder`, `.github/workflows/ci.yml`, README,
  `docs/testing.md`, a new `bench/results/` entry comparing the providers
- Not breaking for `longshore` users: the default build and `features = ["tls"]` behave as today
