<p align="center">
  <picture>
    <source
      media="(prefers-color-scheme: light)"
      srcset="assets/longshore-logo-light.png"
    />
    <img
      src="assets/longshore-logo.png"
      alt="Longshore"
      width="900"
    />
  </picture>
</p>

<p align="center">
  <strong>The fastest object-storage client on the planet.</strong>
</p>

# longshore

longshore is an object storage client written in Rust. It is built to move a
lot of data with little CPU: one thread on Linux `io_uring`, all memory
allocated at start-up, and object bodies sent to the network without being
copied. It talks to Amazon S3, to S3-compatible stores, and to Google Cloud
Storage.

The design follows the TigerBeetle talk
[The Fastest Object Storage Client in the World](https://youtu.be/NJrqmoNpVGg).

## Status

This is a library under development. It is not published as a crate, and it
is meant to become part of a larger project.

| Area | State |
|---|---|
| S3: `store`, `store_no_clobber`, `fetch`, ranged `fetch`, `list`, `list_continue`, `erase` | Works against Amazon S3 and Linode Object Storage, over HTTP and HTTPS |
| Multipart upload: begin, store part, complete, abort | Works against Amazon S3, Linode Object Storage, Google Cloud Storage and versitygw. Each part is one registered buffer |
| `stat`, stores with a content type and metadata, `copy`, `erase_many`, listing with a delimiter, presigned URLs | Work against Amazon S3, Linode Object Storage and Google Cloud Storage. `erase_many` and presigned URLs need signing keys, so they are not available with Google's bearer token |
| Google Cloud Storage | Works against a real bucket, with a bearer token |
| Azure Blob Storage | Request signing only, checked against the Azurite emulator. The store cannot use it yet |
| Bucket addressing | The bucket goes in the host name by default, or in the path when configured, as self-hosted S3-compatible stores commonly expect. Both forms run against Linode Object Storage. Not yet run against MinIO or Ceph |
| Shareable client | `longshore::client`: a handle usable from any thread over one or more independent I/O threads, with async and blocking calls and lent buffers. Store, conditional store, fetch, ranged fetch, erase and multipart upload. Run on Linux 6.8 and 6.14 against the in-tree server, and measured against Amazon S3 |
| `put`, `head`, `get`: one request within a deadline, reported as the store answered it | In the shareable client and the low-level API. Tested in the simulator and on real rings against the in-tree server. Not yet run against a real store |
| IPv4 and IPv6 | The resolver asks for A records, AAAA records or both, and prefers the family the configuration names. When connections to that family fail and the name has addresses in the other, it uses the other. Run over IPv6 against Linode Object Storage |
| Large-segment addresses | Optional, off by default. A store's name can resolve to addresses whose connections get different TCP segment sizes, and on a network card that does not segment in hardware the small ones cost several times more CPU per byte. With the setting on, the client measures each new connection, keeps its connections on the addresses with large segments, and asks DNS again sooner when every address is small. It can be switched while the store runs. Works on Linux 6.1 and later |
| TLS 1.3 | `rustls` does the handshake. After that, records are sealed and opened in this repository with AES-GCM from AWS-LC |
| Checksums | CRC-64/NVME for S3, CRC-32C for Google. A fetch is verified when the store returns a checksum, and both stores do |
| Credentials | Long-lived keys, temporary keys with a session token, or a bearer token. Read from a file and replaced when the file changes, once the store has accepted the new ones unless that check is turned off |

## How it works

A store runs on one thread, driven by `io_uring` completions. There are no
locks and no async runtime on that thread. Sockets and buffers are registered with the
kernel once.

Buffers, connection slots and operation slots are sized from the
configuration and allocated when the client is built. Nothing is allocated
after that, and the tests run under an allocator that aborts if something
is. The TLS handshake is the exception, because `rustls` allocates.

An object body goes from the caller's registered buffer to the network card
with `SEND_ZC`. Requests are signed over the head only, so a body is never
run through a cryptographic hash.

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
| `longshore_crc` | CRC-64/NVME, CRC-32C, base64 | `crc-fast` |
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

## Requirements

- Linux 6.1 or later, on x86-64 or AArch64. Older kernels are not supported.
- A recent stable Rust toolchain.

The code builds on macOS and the simulator tests run there. Anything that
uses `io_uring` has to be cross-compiled and run on a Linux host.

## Build and test

```sh
cargo test --workspace          # unit, simulation and property tests
cargo clippy --workspace --all-targets -- -D warnings
./scripts/test-model.sh         # the client's hand-offs, model checked
./scripts/test.sh               # the tests and the model, then the real-kernel checks
```

`scripts/test-linux.sh` cross-compiles the check programs for
`aarch64-unknown-linux-musl` and `x86_64-unknown-linux-musl`, copies them to
the hosts listed in `~/.config/longshore/test-hosts`, and runs them. That file
has one `<rust-target> <ssh-destination>` per line. If it is missing, the
real-kernel checks are skipped with a warning.

`scripts/test-model.sh` builds `longshore_s3` with its locks, atomics and thread
parking replaced by [loom](https://github.com/tokio-rs/loom)'s, and runs the
hand-offs between callers and I/O threads in every order loom reaches with
one or two forced thread switches. A wake-up that could be lost shows up
there as a thread left waiting. loom is only in that build.

`client_faults`, one of the check programs, makes a running client's rings
fail and tears clients down with work in flight. It is built with the
`fault-injection` feature, which is off everywhere else.

Property tests use [Hegel](https://hegel.dev). The fuzz targets run with
`cargo fuzz` from `fuzz/`.

## Trying it against a store

`s3_uring` shows how to drive the library, and it is also the self-test and
the benchmark. Its source is in `crates/longshore-bench/src/bin/s3_uring/`. On a
Linux host:

```sh
export S3_ENDPOINT=s3.us-east-1.amazonaws.com S3_REGION=us-east-1 S3_BUCKET=my-bucket
export S3_ACCESS_KEY=... S3_SECRET_KEY=...
export S3_SESSION_TOKEN=...               # only with temporary keys
export S3_TLS=1                           # HTTPS; omit for plain HTTP
export S3_PATH_STYLE=1                     # bucket in the path: /bucket/key on the endpoint
export S3_FAMILY=prefer6                  # 4 (the default), 6, prefer6 or prefer4
export S3_PREFER_LARGE_SEGMENTS=2         # prefer large-segment addresses, using at least 2

s3_uring selftest                         # store, conditional store, fetch, range, erase
s3_uring put --conns 64 --secs 10         # throughput, 8 MiB objects by default
s3_uring get --conns 64 --secs 10
s3_uring put --conns 128 --threads 4 --cpu 2 --cpu-step 2   # four independent clients, pinned to CPUs 2, 4, 6, 8
s3_uring multipart                        # three 8 MiB parts in parallel, then an abort
s3_uring objects                          # stat, metadata, copy, delimiter listing, batch erase
s3_uring list some/prefix/
```

For Google Cloud Storage, set `S3_VENDOR=gcs` and
`S3_ENDPOINT=storage.googleapis.com`, and give `S3_BEARER_TOKEN` in place of
the keys.

The store's own API is low level. The caller owns the event loop, gives the
store registered buffers, submits each operation with a token, and collects
outcomes with `poll`. The simulation tests in `crates/longshore_s3/tests/store/` are
the shortest complete examples.

## Using it from other threads

`longshore::client` puts the store behind a handle that any thread can
share. It starts one I/O thread, or as many as asked for. Each I/O thread is
a complete store with its own connections and buffers, and they share
nothing. One thread fills a fast network card with plain uploads. More are
for HTTPS and downloads, which one core bounds.

```rust,no_run
use longshore::client::Builder;
use longshore::store::creds::Credentials;

let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
let mut settings = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials);
settings.tls = true;
settings.port = 443;
settings.threads = 4; // four independent I/O threads
let client = settings.build().unwrap();

// A body lives in a registered buffer borrowed from the client, so it is
// never copied. The operation takes the buffer and hands it back.
let mut body = client.buffer_blocking();
body.as_mut_slice()[..5].copy_from_slice(b"hello");
let (stored, body) = client.store("greeting", body, 5).wait();
stored.unwrap();
let (fetched, body) = client.fetch("greeting", body).wait();
assert_eq!(fetched.unwrap(), Some(5));
assert_eq!(&body.as_slice()[..5], b"hello");
```

Every operation returns a value that is a `Future` and also has a blocking
`wait`, so the same calls work under any async executor and from plain
threads. When no buffer or operation slot is free a call waits. It has
store, conditional store, fetch, ranged fetch, erase, multipart upload
and the reporting `put`, `head` and `get` so far. Listing, `stat` and the other store operations are on the
low-level API only.

An object larger than one buffer goes up in parts. `upload_begin` returns
an id, each `upload_part` stores one buffer under a part number and
returns what completing needs, and `upload_complete` joins them. Parts are
separate operations, so several can be in flight at once and on different
I/O threads. The caller holds the id and the parts, and an upload that is
neither completed nor aborted keeps its parts in the store. Whether parts
carry checksums follows the `checksums` setting of the handle, so begin an
upload and store its parts through handles with the same setting.

`client::Worker` is one I/O thread's logic without the thread, for a
caller that already runs an event loop per core.

### When the caller must know exactly what happened

`store` and `fetch` decide things for the caller: they retry, and a
conditional store that was refused after a retry compares checksums to tell
its own write from someone else's. A caller that has to account for every
request, such as an archive that may delete its local copy only once the
stored one is proven, needs the opposite. `put`, `head` and `get` take a
`Budget` of attempts and a deadline, send what it allows, and return a
`Report` of what the store answered. They do not turn a status into
success or failure.

```rust,no_run
# use longshore::client::{Budget, Builder};
# use longshore::store::creds::Credentials;
# use longshore::store::report::{Encryption, End, Expect, Put};
# use longshore::store::s3::Checksum;
# use std::time::{Duration, Instant};
# let credentials = Credentials::new("ACCESS_KEY", "SECRET_KEY").unwrap();
# let client = Builder::new("my-bucket.s3.us-east-1.amazonaws.com", "us-east-1", credentials).build().unwrap();
# let (mut body, len, digest) = (client.buffer_blocking(), 5, [0u8; 32]);
// One request, started before the deadline or not at all.
let budget = Budget::once(Instant::now() + Duration::from_secs(10));
let options = Put {
    no_clobber: true,                        // If-None-Match: *
    encryption: Some(Encryption::Aes256),    // ask the store to encrypt it
    checksum: Checksum::Sha256(digest),      // the caller's SHA-256 of the body
};
let (put, body) = client.put("clips/0001.mp4", body, len, &options, budget).wait();
match put.expect("an error means nothing was sent").end {
    // Any status. 200 stored it, 412 means an object was already there.
    End::Answered(answer) => {
        // Bind every later read to the object the store named.
        let expect = Expect { etag: answer.etag, version: answer.version };
        let head = client.head("clips/0001.mp4", &expect, budget).wait();
        let (part, body) = client.get("clips/0001.mp4", Some((0, 99)), &expect, body, budget).wait();
    }
    // Nothing left this client. The store cannot have acted.
    End::NotSent(cause) => {}
    // The request went out and no answer came. The object may be there.
    End::Unknown(cause) => {}
}
```

- The result, and the buffer with it, come only when the I/O thread has
  finished with the operation and the kernel with the buffer. A deadline
  cancels a request in progress, and the result arrives when the kernel has
  let go of it, which is shortly after the deadline and not at it: the I/O
  thread looks every 50 ms. There is no earlier "timed out" return, so a
  caller that counts operations in progress can count until the result.
- `Err` means nothing was sent. Once a request may have gone out the result
  is a report.
- An answer holds what the response carried: status, `ETag`, version id,
  request id, length, `Content-Range`, encryption and checksums. A header
  the store did not send is `None`. Nothing is filled in from what was
  asked for, so a store that ignores a request for encryption shows as an
  answer without it.
- `Report::request` is evidence of the request that was handed to the
  kernel: its method, its date, the names of its signed headers and the
  SHA-256 of the canonical request the signature covers, which
  `sigv4::canonical_request_sha256` rebuilds from the values expected. It
  holds no key, signature or session token, and is absent when nothing was
  sent.
- A store takes one kind of checksum per object, so a SHA-256 is sent in
  place of the CRC-64, not beside it. The store checks the body against it.
  This client still never hashes a body with SHA-256: the caller supplies
  the digest.
- With more than one attempt, a request that got no answer is counted in
  `Report::unanswered` and never hidden: a 412 that follows one may be this
  call's own earlier write, and the caller is the one to find out.

`examples/verified_write.rs` in `longshore_s3` is a complete program.

Settings on the builder cover what a long-running program needs:

- `credentials_file`: a file the client rereads its keys from. Temporary
  keys expire, and a client that only has the keys it was built with stops
  working when they do. Each I/O thread rereads the file on its own
  interval, tries new keys with one request, and adopts them if the store
  accepts them. That request is a listing of one key, of the whole bucket
  unless `credentials_validate` names a prefix. Keys that may list only
  under a prefix need the prefix set, or they are never adopted.
  `Client::credential_reloads` counts reloads adopted and failed.
- `bucket_in_path`: the bucket in the path, for stores that expect it there.
- `checksums`: whether a store sends a CRC-64 of its body. It is on by
  default and is one more pass over the data. Turning it off affects plain
  stores and multipart uploads. A conditional store always sends one, because when a retry
  is refused as already existing, the checksum is how the client tells its
  own earlier write from another writer's object.

The client's ordinary path is tested in the simulator and on real kernels.
So is what happens when an I/O thread's ring fails. `client_faults` makes
rings fail under a client that callers are using, on Linux 6.8 and 6.14, in
two ways: the next wait reports an error, or the kernel refuses the ring
from then on. Each time, the callers of the failed thread get their
operations back as closed, the other threads carry on, and once every
thread has failed every call still returns. It also builds and drops 2,000
clients with work in flight. After both, the I/O threads and their
descriptors are gone. The hand-offs between callers and I/O threads are
model checked. Those failures are ones a test causes. A ring failing by
itself has not been seen. The client's longest run against a store is 30
minutes (`m22-client-and-soak.md`).

## Running in containers

A container meets two limits that a plain Linux host does not. Both were
found by running the ring checks in a Kubernetes pod with a non-root user,
every capability dropped and the default seccomp profile, on containerd
2.3 and Linux 6.8.

The first is seccomp. The default profile of the container runtime refuses
the three `io_uring` system calls, so creating a ring fails with
"Operation not permitted". The pod needs a profile that is the runtime's
default plus `io_uring_setup`, `io_uring_enter` and `io_uring_register`.
`deploy/seccomp/containerd-2.3-x86_64-io-uring.json` is that profile for
containerd 2.3 on x86-64, for a container with no capabilities. It is the
profile the runtime generated for the test pod with one rule added, and the
ring checks pass under it, zero-copy sends included. A runtime's default
changes between versions and architectures, so for anything else start
from that runtime's own default and add the same rule.

In Kubernetes the file goes in the kubelet's seccomp directory on every
node that may run the pod, usually `/var/lib/kubelet/seccomp`, and the pod
names it:

```yaml
securityContext:
  seccompProfile:
    type: Localhost
    localhostProfile: containerd-2.3-x86_64-io-uring.json
```

Node configuration management or the Security Profiles Operator can place
the file. With Docker the same file is passed as
`--security-opt seccomp=<file>`. A node can also turn `io_uring` off for
everything with the `kernel.io_uring_disabled` setting, on Linux 6.6 and
later, and no profile changes that.

The second is locked memory. longshore registers its buffers with the
kernel, which locks them in memory, and that is what lets a body go out
without a copy. A process may lock only as much as its `RLIMIT_MEMLOCK`
allows. The test pod had a limit of 8 MiB that it could not raise, and
building the client failed with "Out of memory". That error now goes on to
say that `RLIMIT_MEMLOCK` is too low. The limit has to be raised
on the node. A chart cannot do it, and the `IPC_LOCK` capability is not
open to a pod under the `restricted` Pod Security level.

A container inherits the limit from the container runtime's service. On
the test node that was `k3s-agent`, with the systemd default of 8 MiB.
This drop-in and a restart of the service raised it:

```ini
# /etc/systemd/system/k3s-agent.service.d/10-memlock.conf
[Service]
LimitMEMLOCK=infinity
```

A pod started after that, with the same restricted settings and the
profile above, ran the ring checks and the client with registered buffers
and zero-copy sends. Pods that were already running kept the old limit.
The same setting goes on `containerd.service` where containerd runs as its
own service. It applies to every pod on the node, and every node that may
run the pod needs it. Managed Kubernetes services were not tested.

What is locked is each I/O thread's whole buffer pool: the buffers lent to
callers and the ones its connections use. That is most of the memory the
client allocates, so a limit that is not `infinity` has to be about the
size of the client. Locked memory is counted per user id on a node, so two
pods that run as the same user share one limit. The memory is allocated at
start-up, so the container's memory limit has to cover it too.

## Measurements

All of these use 8 MiB objects, and one client thread unless a row says
otherwise. GB means 10^9 bytes.
Each row names its file in `bench/results/`, which has the full conditions.

### Against Amazon S3

One client thread, from instances in the same region as the bucket, with
the preference for large-segment addresses on. Medians of five runs, and
of eight for HTTPS PUT (`m20-preference-and-comparison.md`).

| Request | Graviton4, 300 Gbit/s card | AMD Zen 5, 200 Gbit/s card | Limit |
|---|---|---|---|
| PUT, plain HTTP | 36.7 GB/s | 22.4 GB/s | Graviton4: the network card. AMD: 22.4 GB/s is 179 Gbit/s on a 200 Gbit/s card with the client thread at 0.80, so neither the card nor the thread is clearly the limit |
| PUT with a CRC-64 checksum on every body | 16.4 GB/s | 13.3 GB/s | The client's core |
| GET | 4.4 GB/s | 7.9 GB/s | The client's core |
| HTTPS PUT, 3 MiB send batch | 5.2 GB/s | 7.3 GB/s | The client's core |
| HTTPS GET | 2.7 GB/s | 5.1 GB/s | The client's core |

Plain PUT fills the 300 Gbit/s card with half of the client's thread idle,
and keeps about four of the machine's cores busy, most of it kernel network
work on other cores. Without the preference the same instances give a
median of 23.2 and 11.8 GB/s for plain PUT, and runs differ by 45% or more,
because S3's name resolves to some addresses whose connections get small
TCP segments (`m18-s3-aws-large-instances.md`).

The rows bound by the client's core grow with more clients. Separate
client processes side by side, each on its own core and sharing nothing:

| Request | Graviton4, 8 clients | AMD, 4 clients | AMD, 8 clients |
|---|---|---|---|
| HTTPS PUT | 34.5 to 36.2 GB/s | 24.0 GB/s | 16.7 GB/s, one run |
| GET | 26.1 to 26.5 GB/s | 22.0 to 22.1 GB/s | 24.8 to 26.1 GB/s |
| HTTPS GET | 17.6 to 17.9 GB/s | 14.7 to 14.8 GB/s | 21.6 to 22.1 GB/s |

Four clients fill the AMD instance's card over HTTPS, and eight were slower
there than four. These are processes started by hand, three runs per cell
unless marked.

`longshore::client` does the same inside one process, with one I/O thread
per core. Medians of three runs (`m22-client-and-soak.md`):

| Request | Graviton4, 1 I/O thread | 4 | 8 | AMD, 1 I/O thread | 2 | 4 |
|---|---|---|---|---|---|---|
| HTTPS PUT with a CRC-64 checksum | 4.4 GB/s | 17.1 | 31.1 | 6.2 GB/s | 13.1 | 23.9 |
| HTTPS PUT, checksums off | 5.3 | 20.9 | 37.6 | 7.5 | 15.5 | 25.0 |
| GET | 4.6 | 15.6 | 26.7 | 8.0 | 14.5 | 22.4 |
| HTTPS GET | 2.7 | 10.1 | 18.0 | 5.3 | 9.5 | 15.0 |

The client sends a checksum with every upload unless told not to. With the
same work asked of both, it is never more than 4% below the store driven
directly and is up to 7% above it. One I/O thread with checksums off stores
37.6 GB/s over plain HTTP on the Graviton4 instance and 25.0 on the AMD one.
In a 30 minute HTTPS upload on four I/O threads, with every connection to S3
closed every 40 seconds and all traffic to it dropped for 5 seconds after
every fourth of those, the client stored 3.4 million objects and returned no
error to its callers.

Object size matters below about 4 MiB. On 128 connections one thread
stores 2.9 GB/s of 1 MiB objects and 13.9 GB/s of 64 MiB objects. The time
a request takes fits about 32 ms plus 9 ms per MiB, so the fixed 32 ms
dominates small objects (`m21-object-sizes.md`).

### Compared with other clients

s5cmd 2.3.0 and the AWS CLI 2.40.2 with its CRT transfer client, on the
Graviton4 instance against the same bucket, moving 8,000 objects of 8 MiB
held in RAM. Best run of each, with the cores busy across the machine.

| Request | s5cmd | AWS CLI with CRT | One client here | Eight clients here |
|---|---|---|---|---|
| PUT over HTTP | 22.1 GB/s on 72.0 cores | 6.5 on 5.3 | 36.9 on 3.9 | Not run: one fills the card |
| PUT over HTTPS | 14.3 on 54.0 | 6.6 on 7.5 | 5.3 on 1.3 | 36.2 on 13.5 |
| GET over HTTP | 11.0 on 42.4 | 4.0 on 5.6 | 4.4 on 1.2 | 26.5 on 10.9 |
| GET over HTTPS | 11.5 on 43.0 | 4.0 on 6.2 | 2.8 on 1.1 | 17.9 on 10.5 |

One client thread is the fastest of these for plain PUT and uses the fewest
cores per byte for every request. For HTTPS and for GET, s5cmd on 42 to 54
cores in its best runs moves more in total than one client thread does.
Four clients side by side pass it for HTTPS PUT and plain GET on both
machines and for HTTPS GET on the AMD machine. For HTTPS GET on the
Graviton4 machine four clients give 9.9 to 10.0 GB/s against s5cmd's 11.2
to 11.5, and eight clients give 17.6 to 17.9. Those tools read and write files, which costs them a copy
that our benchmark does not make, and their runs lasted 3.0 to 17.2 seconds.
The same file has the AMD figures and the conditions.

An earlier 32-core Graviton4 instance with a 100 Gbit/s card is in
`m14-s3-aws-us-east-1.md`. Plain PUT and PUT with checksums both filled
that card at 12.2 GB/s.

### Between two machines

The client on one DGX Spark (Arm Cortex-X925) sends to a second one over
200 Gbit/s Ethernet. The peer is a small program of ours that discards what
it receives, so these figures show what the client costs and say nothing
about any store. Medians of five runs (`m15-dgx-repeated-with-machine-cpu.md`).

| Request | Result | Limit | Cores busy on the client machine |
|---|---|---|---|
| PUT, plain HTTP | 24.5 GB/s | The link | 2.5 |
| PUT with CRC-64 checksums | 14.7 GB/s | The client's core | 1.5 |
| GET, plain HTTP | 6.4 GB/s | The client's core | 1.5 |
| HTTPS PUT | 5.8 GB/s | The client's core | 1.3 |
| HTTPS PUT, faster AES routine | 7.6 GB/s | The client's core | 1.3 |
| HTTPS GET, faster AES routine | 4.1 GB/s | The client's core | 1.5 |

The "faster AES routine" rows use a build made with
`scripts/build-fast-aes-aarch64.sh`. AWS-LC has an 8-way AES-GCM routine for
Arm that it only uses on processors it recognises, and the Cortex-X925 is
not one of them. The script fixes AWS-LC's processor capabilities when it is
compiled. A binary built that way crashes on a processor without those
instructions, so it is for hardware you know. x86-64 and Graviton do not
need it.

### AES-GCM by processor

HTTPS speed can follow the processor's AES speed: in a profile of an HTTPS
upload on the DGX Spark, two thirds of the client's core is inside the
cipher. Sealing TLS records, one core, in GiB/s (`m13-aes-gcm-by-cpu.md`):

| Processor | GiB/s |
|---|---|
| AMD EPYC 9R05, Zen 5 (AWS `m8azn`) | 25.6 |
| AMD EPYC 9R45, Zen 5 (AWS `c8a`) | 22.2 |
| Intel Xeon 6975P-C (AWS `c8i`) | 16.2 |
| Arm Cortex-X925, faster routine (DGX Spark) | 12.2 |
| AMD EPYC 9R14, Zen 4 (AWS `c7a`) | 10.4 |
| Arm Cortex-X925, default build | 7.8 |
| AWS Graviton4 (AWS `c8g`) | 6.1 |

On the AMD `m8azn` instance HTTPS upload to S3 is 7.3 GB/s with large
segments and a 3 MiB send batch, and 3.5 GB/s through every address S3
returns with a 1 MiB batch, both with the client's thread full
(`m20-preference-and-comparison.md`). The profiles in
`m18-s3-aws-large-instances.md` show where the core goes in each case:
about half in AES-GCM with large segments, and mostly in the kernel's
transmit path with mixed addresses. HTTPS upload has not been measured
end to end on the Intel processor.

### Compared with the talk

The talk's figures are from its slides. Its hardware differs from ours and
the slides do not describe the method in detail, so read this as a rough
placement.

| Measurement | Talk | Here |
|---|---|---|
| PUT, plain HTTP | 25.2 GB/s per core | 36.7 GB/s to S3, with a 300 Gbit/s card full |
| PUT with CRC-64 checksums | 4.3 GB/s, the speed of its checksum | 16.4 GB/s to S3 |
| HTTPS PUT | No end-to-end figure. 7.4 GB/s for the cipher alone | 5.2 to 7.3 GB/s to S3, 7.6 GB/s between our machines |
| GET | 4.7 GB/s | 4.4 to 7.9 GB/s from S3, 6.4 GB/s between our machines |
| CRC-64 alone | 4.3 GB/s | 88 GB/s, and about 30 GB/s inside a PUT (`m5-checksums.md`) |
| AES-GCM alone | 7.4 GB/s | 6.5 to 27 GB/s, depending on the processor |

The talk reports "per core". Between our own machines plain PUT fills the
core the client runs on, and about 1.3 cores of work at 16 connections and
1.6 at 32 run on other cores, most of it in the kernel. To S3 at
36.7 GB/s the client's thread is half idle and about 3.4 more cores are
busy elsewhere on the machine. The slides do not say whether the
talk counts that work, so we cannot tell which of our numbers matches
theirs.

Request rates for small operations are in `m9-s3-ops-linode.md` and
`m14-s3-aws-us-east-1.md`. In both, the store's latency sets the rate and
the client is mostly idle.

## Limits

- Plain PUT fills a 200 Gbit/s link between our own machines and 100 and
  300 Gbit/s cards to S3, and reaches about 90 percent of the 200 Gbit/s
  card on the AMD instance. Where one thread
  stops is unknown. The S3 figures need the preference for large-segment
  addresses, which is off by default.
- With one client everything except plain PUT is bound by that client's
  core. With several clients the network card becomes the limit: eight
  Graviton4 HTTPS PUT clients, four AMD HTTPS PUT clients and eight AMD GET
  clients.
- The shareable client was run against S3 for 30 minutes with connections
  being broken, not for days. The failure of an I/O thread itself is made
  to happen by a test, in the simulator and on Linux 6.8 and 6.14. It has
  not been seen to happen by itself, and was not part of that 30 minute run.
- The model checker runs the client's hand-offs with at most two forced
  thread switches, and one in the tests of four threads. An order that
  needs more is not reached.
- On x86-64 the only real-store measurements are from one AMD instance
  type. Intel has not been measured against a store.
- On Linux 6.1 the client was run for minutes, not hours. Seen once by hand
  there, with no output kept: a connection the client had closed stayed
  listed as established for about 30 seconds and then went. On Linux 7.0 it
  went at once. Linux 6.6 has not been run.
- TLS keys are never updated in place. A connection is retired after 2^23
  records and the next request handshakes again. If a server updates its
  keys in the middle of a connection, the request is retried on a new one.
- If a server sends extra bytes after a complete response and they arrive
  in a later read, the client does not notice. It does catch them when they
  arrive in the same read.
- One `store` or `fetch` moves at most one buffer. A larger object goes up
  as a multipart upload and comes down as ranged fetches, and the caller
  drives both.
- A presigned URL does not check when temporary credentials expire. One
  signed with them stops working when they do, whatever its own expiry.
- A multipart upload that is neither completed nor aborted keeps its parts
  in the store. A bucket rule that expires incomplete uploads is the backstop.
- `put`, `head` and `get` have been run against the in-tree server only,
  in the simulator and on real rings. Which of `If-None-Match`, the
  encryption header and the SHA-256 checksum a given S3-compatible store
  honours and returns has not been checked against any real store.
- A request counts as sent once its head is handed to the kernel. That does
  not prove the store received it, so some requests reported as of unknown
  outcome never arrived. "Not sent" is certain.
- A deadline does not bound the time a call waits for a free buffer or
  operation slot. A request that reaches the I/O thread after its deadline
  is not sent.
- Azure Blob Storage cannot be used through the store yet, and its signing
  has not been tried against a real account.
- HTTP/1.1 only.
- Preferring large-segment addresses puts a store's connections on fewer
  of its addresses. The setting keeps a minimum number in use, and a store
  may still limit or shed load per address.
- Measuring a connection's segment size costs four system calls when the
  connection opens, and none after that.
- After connections to the preferred address family fail, the client stays
  on the other family until a connection to that one fails too. It does not
  try the preferred family again on its own.
- IPv6 has not been run against Amazon S3's dual-stack endpoints, only
  against Linode Object Storage and over loopback.
- Each measurement table states how many runs are behind its figures. The
  comparison with the talk takes its figures from those tables and from the
  files it names. Anything stated with no count is a single run.

## License

Apache License 2.0. See `LICENSE`.
