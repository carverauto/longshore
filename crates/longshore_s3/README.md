# longshore_s3

[![crates.io](https://img.shields.io/crates/v/longshore_s3.svg)](https://crates.io/crates/longshore_s3)
[![docs.rs](https://img.shields.io/docsrs/longshore_s3)](https://docs.rs/longshore_s3)
[![License](https://img.shields.io/crates/l/longshore_s3.svg)](https://github.com/carverauto/longshore/blob/main/LICENSE)

The object store of longshore: operations against Amazon S3,
S3-compatible stores and Google Cloud Storage, one thread per ring, with
memory allocated at start-up and bodies sent without a copy.

## Two ways in

- `client` is the one most programs want. A `Client` is a handle any
  thread can share, over one or more I/O threads that share nothing. Its
  calls are futures that can also block, and bodies live in buffers it
  lends.
- `s3` is the store itself, single-threaded and written against the `Io`
  trait of `longshore_common`. The caller owns the event loop, submits
  each operation with a token and collects outcomes by polling. It runs
  the same on the kernel and in the simulator, which is how its retry and
  failure paths are tested. `client::Worker` is one I/O thread of the
  client without the thread, for a caller that already runs a loop per
  core.

## Example

```rust,no_run
use longshore_s3::client::Builder;
use longshore_s3::creds::Credentials;

let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
let client = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials)
    .tls(true)
    .port(443)
    .threads(4) // four independent I/O threads
    .build()
    .unwrap();

// The body lives in a registered buffer borrowed from the client, so the
// client never copies it. Each operation takes the buffer and hands it back.
let mut body = client.buffer_blocking();
body.as_mut_slice()[..5].copy_from_slice(b"hello");
let (stored, body) = client.store("greeting", body, 5).wait();
stored.unwrap();
let (fetched, body) = client.fetch("greeting", body).wait();
assert_eq!(fetched.unwrap(), Some(5));
assert_eq!(&body.as_slice()[..5], b"hello");
```

## What it gives

- Store, conditional store, fetch, ranged fetch, erase and multipart
  upload on the client. The store adds listing, batch erase, `stat` and
  presigned URLs, and `options` carries headers such as content type,
  metadata, storage class and server-side encryption.
- `put`, `head` and `get`, which take a budget of attempts and a deadline
  and report exactly what the store answered (`report`), for a caller that
  must account for every request.
- By default a checksum goes with every upload (CRC-64/NVME on S3,
  CRC-32C on Google Cloud Storage), and each download is checked against
  the checksum the store returns.
- Requests signed with SigV4 over the head only: a body is never hashed.
- `retry`, which says what is tried again and how often.
- `creds`: long-lived keys, temporary keys with a session token, or a
  bearer token for Google Cloud Storage, read from a file and reread on a
  schedule the client sets.

## Features

| Feature | What it turns on |
|---|---|
| `aws-lc` (default; `tls` is the same) | HTTPS, with AWS-LC sealing records without a copy |
| `ring` | HTTPS with ring; no AWS-LC in the build |
| `rustls` | HTTPS entirely from rustls |
| `fault-injection` | `Client::inject_fault`, which breaks a running ring on purpose; off in every ordinary build |

With several providers compiled in, `Builder::tls_provider` picks one.
With `default-features = false` and none, the client speaks plain HTTP;
asking for TLS then makes `build` fail with an error that names the
feature.

## Platforms

Linux 6.1 or later uses `io_uring`, and `Builder::portable(true)` selects
`epoll`. macOS runs on `kqueue`. Other targets build the crate and run
the simulator, while `Builder::build` returns `BuildError::Unsupported`.
In a container, `io_uring` needs a seccomp profile that allows it and a
locked-memory limit as large as the client's buffers.

## Part of longshore

longshore gathers this crate and ten others under one name, and re-exports
it as `longshore::store` (with `longshore::client` and `longshore::crypto`).
Most programs depend on [longshore](https://crates.io/crates/longshore)
instead. The crates are released together: depend on all of them at the
same version.

- API documentation: <https://docs.rs/longshore_s3>
- Guides: [The shareable client](https://github.com/carverauto/longshore/blob/main/docs/client.md),
  [Running in containers](https://github.com/carverauto/longshore/blob/main/docs/containers.md),
  [Status](https://github.com/carverauto/longshore/blob/main/docs/status.md)
- Repository: <https://github.com/carverauto/longshore>

## License

Apache License 2.0. See [`LICENSE`](https://github.com/carverauto/longshore/blob/main/LICENSE).
