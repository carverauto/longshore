# The shareable client

Files named in backticks, such as `m20-preference-and-comparison.md`, are in [`bench/results/`](../bench/results/), each with the conditions it was measured under.

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

## When the caller must know exactly what happened

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

## Settings

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

## How it is tested

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
itself has not been seen. The client's longest run against a store is
eight hours (`m23-client-eight-hours.md`).
