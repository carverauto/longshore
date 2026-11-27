# longshore_stream

[![crates.io](https://img.shields.io/crates/v/longshore_stream.svg)](https://crates.io/crates/longshore_stream)
[![docs.rs](https://img.shields.io/docsrs/longshore_stream)](https://docs.rs/longshore_stream)
[![License](https://img.shields.io/crates/l/longshore_stream.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

One connection's byte stream over the `Io` trait of `longshore_common`,
plain TCP or TLS 1.3, for clients and servers alike.

A caller asks a `Stream` to send bytes from a buffer or receive bytes into
one, and sees the same interface and plaintext lengths whether the
connection is encrypted or not. The HTTP client and `longshore_tcp` both
run their connections on it.

## What it gives

- `Stream`: a plain stream hands sends, receives and idle checks straight
  to `Io`. A TLS stream runs the rustls handshake, then seals records from
  the source buffer into a send buffer and opens received records into the
  destination buffer, through `longshore_tls`.
- `Step`, which tells the owner what the stream needs next.
- `IdleCheck`: what a connection that should be silent turned out to
  hold, so that a pool reuses only a clean one.
- `RingBytes`, `RingDst` and `RingStep`, for bytes received into the
  backend's buffer ring.
- `TlsError`, and `KEY_RECORDS_RETIRE`: past this many records in either
  direction, a connection should carry no further requests.

## Features

| Feature | Records sealed and opened by |
|---|---|
| `aws-lc` (default; `tls` is the same) | AWS-LC, with no copy |
| `ring` | ring, in place; no AWS-LC in the build |
| `rustls` | rustls's own record layer, which allocates a buffer per sealed record |

With `default-features = false` and no provider, only plain streams are
built. The providers turn on `tls-core`; a manifest does not name it on its
own.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::stream`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead. The crates are
released together: depend on all of them at the same version.

- API documentation: <https://docs.rs/longshore_stream>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
