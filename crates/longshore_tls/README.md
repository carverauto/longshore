# longshore_tls

[![crates.io](https://img.shields.io/crates/v/longshore_tls.svg)](https://crates.io/crates/longshore_tls)
[![docs.rs](https://img.shields.io/docsrs/longshore_tls)](https://docs.rs/longshore_tls)
[![License](https://img.shields.io/crates/l/longshore_tls.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

TLS 1.3 for longshore: rustls runs the handshake, and AES-GCM from AWS-LC,
ring or rustls seals and opens the records. Neither part does I/O.

## What it gives

- `record`, the record layer. It seals application data from the
  caller's buffer straight into a send buffer, and opens received records
  straight into the caller's buffer. It allocates nothing.
- `handshake`, which wraps rustls to set up a connection, client or
  server, and nothing else. It allocates: the one exception to
  longshore's rule that nothing allocates after start-up. When it
  finishes, it installs its keys in `record`.
- `provider`: `Provider` names the library that seals and opens records,
  chosen per connection when several are compiled in.
- `wide_gcm` (with `aws-lc`): AWS-LC's 8-way AES-GCM on the AArch64 cores
  it is faster on but does not recognise by itself.

## Features

At least one provider must be on. The default is `aws-lc`.

| Feature | Records sealed and opened by | Handshake crypto |
|---|---|---|
| `aws-lc` (default) | AWS-LC's scatter/gather calls, with no copy | AWS-LC |
| `ring` | ring, in place, one copy | ring |
| `rustls` | rustls's own record encryption, which allocates a buffer per sealed record | ring |

One client core seals 10.2 GB/s with AWS-LC, 6.8 with ring and 6.2 with
rustls (Apple M3 Max, AES-128-GCM, 16 KiB records);
[`m29-tls-providers.md`](https://github.com/carverauto/longshore/blob/main/bench/results/m29-tls-providers.md)
has the conditions.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::tls`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead and turn TLS on
with `.tls(true)`. The crates are released together: depend on all of
them at the same version.

- API documentation: <https://docs.rs/longshore_tls>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
