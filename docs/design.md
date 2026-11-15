# How it works

A store runs on one thread, driven by `io_uring` completions. There are no
locks and no async runtime on that thread. Sockets and buffers are registered with the
kernel once.

Buffers, connection slots and operation slots are sized from the
configuration and allocated when the client is built. Nothing is allocated
after that, and the tests run under an allocator that aborts if something
is. The TLS handshake is the exception, because `rustls` allocates.

The client makes no copy of an upload's body. By default it reads the body
once, from the caller's registered buffer, to compute the checksum that goes
with the upload; `checksums = false` turns that pass off. Over plain HTTP the
body is then sent from that buffer. Over HTTPS sealing reads the body once
more and writes the records into the connection's send buffer, and those
records are what is sent. Whether a send is zero-copy is decided for each
send, not for the body. A send of 64 KiB or more is submitted with `SEND_ZC`,
and a smaller one is an ordinary send that the kernel copies. When a send
completes short, the rest is sent again. Over plain HTTP that remainder is a
zero-copy send only if it is still 64 KiB or more. Over HTTPS the remainder
is always an ordinary send that the kernel copies, however large it is.
Requests are signed over the head only, so a body is never run through a
cryptographic hash.

DNS, HTTP, the TLS record layer and the store's retry logic are written
against an `Io` trait and do no I/O themselves. The same code runs on the
real kernel and in `longshore::sim`, a simulator with virtual sockets, files and
time. The simulator is seeded, so a failing run replays exactly, and it can
inject resets, short writes, partial reads, flipped bits and delays.

Timers fire at intervals the client chooses. It refreshes DNS, retires
connections and rereads credentials on its own schedule, whatever a server
suggests.

## Layout


The library is six crates in `crates/`, and a seventh that gathers them.

| Crate | Contents | Uses |
|---|---|---|
| `longshore_common` | `io`: the `Io` trait, the `io_uring` backend on raw system calls, and the allocation guard. `sim`: the simulator, behind the `sim` feature | Nothing outside the standard library |
| `longshore_dns` | Resolver for A and AAAA records over UDP | `longshore_common` |
| `longshore_crc` | CRC-64/NVME, CRC-32C, base64, with their own kernels for x86-64 and AArch64 | Nothing outside the standard library |
| `longshore_tls` | TLS 1.3 record layer and the `rustls` handshake wrapper | `longshore_common`, `aws-lc-sys`, `rustls`, `webpki-roots` |
| `longshore_http_client` | HTTP/1.1 client with a connection pool and chunked decoding, a stream that is plain or TLS, and a server used by the tests | `longshore_common`, `longshore_tls` |
| `longshore_s3` | The store: operations, SigV4 and Azure Shared Key signing, retries, listing, credentials. `client`: a handle any thread can share, over one or more I/O threads that each run a store, with lent buffers. `crypto`: SHA-256 and HMAC-SHA256 | The five above, `ring` |
| `longshore` | No code. It re-exports the others as `longshore::io`, `sim`, `dns`, `checksum`, `tls`, `http`, `store`, `client` and `crypto` | The six above |

A protocol crate does no I/O of its own. It submits operations to an `Io`
and is fed their completions, so each one runs the same on the kernel and
in the simulator. Every outside dependency is named once, in the workspace
`Cargo.toml`.

`crates/longshore-bench` has the benchmarks and the programs that check the
real kernel path. It is not published.

`deploy/seccomp/` has a seccomp profile that allows `io_uring` in a
container.

`fuzz/` has fuzz targets for the parsers. `bench/results/` has every
measurement with the conditions it was taken under, and `bench/README.md`
explains how to run the benchmarks and summarise repeated runs.
