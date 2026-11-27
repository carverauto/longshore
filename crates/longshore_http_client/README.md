# longshore_http_client

[![crates.io](https://img.shields.io/crates/v/longshore_http_client.svg)](https://crates.io/crates/longshore_http_client)
[![docs.rs](https://img.shields.io/docsrs/longshore_http_client)](https://docs.rs/longshore_http_client)
[![License](https://img.shields.io/crates/l/longshore_http_client.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

HTTP/1.1 for the longshore object storage client, over plain TCP or TLS
1.3, with a fixed pool of connections.

The client copies no body. It writes each request head into the
connection's own buffer, sends the request body straight from the
caller's buffer, and receives the response body straight into the
caller's buffer. It moves only the few response-body bytes that arrive in
the same read as the head.

## What it gives

- `head` writes request heads into a buffer the caller provides and
  parses response heads out of one. It does no I/O, allocates nothing, and
  enforces explicit limits.
- `chunked` decodes chunked transfer encoding in place: the body ends up
  whole at the front of the buffer it arrived in.
- `client` is the connection pool, one request per connection at a time,
  each connection plain or TLS. The owner routes `Io` completions to
  `Client::on_completion` and calls `Client::tick` at its own pace, which
  enforces the request timeouts.
- `server` is a scriptable object server that the tests and benchmarks
  run against.

`client` and `server` use only the `Io` trait of `longshore_common`, so
they run the same on the kernel and in the simulator.

## Features

TLS comes from one of three providers. The default is `aws-lc`.

| Feature | Records sealed and opened by |
|---|---|
| `aws-lc` (default; `tls` is the same) | AWS-LC, with no copy |
| `ring` | ring, in place; no AWS-LC in the build |
| `rustls` | rustls's own record layer, which allocates a buffer per sealed record |

With `default-features = false` and no provider, the client speaks plain
HTTP only. The providers turn on `tls-core`; a manifest does not name it on its
own.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::http`. Most programs depend on
[longshore](https://crates.io/crates/longshore) instead, whose object store
runs on this client. The crates are released together: depend on all of
them at the same version.

- API documentation: <https://docs.rs/longshore_http_client>
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
