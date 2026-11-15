# Limits

What has not been done, measured or proven. Files named in backticks, such as `m20-preference-and-comparison.md`, are in [`bench/results/`](../bench/results/), each with the conditions it was measured under.

- Plain PUT fills a 200 Gbit/s link between our own machines and 100 and
  300 Gbit/s cards to S3, and reaches about 90 percent of the 200 Gbit/s
  card on the AMD instance. Where one thread
  stops is unknown. The S3 figures need the preference for large-segment
  addresses, which is off by default.
- With one client everything except plain PUT is bound by that client's
  core. With several clients the network card becomes the limit: eight
  Graviton4 HTTPS PUT clients, four AMD HTTPS PUT clients and eight AMD GET
  clients.
- The shareable client was run against S3 for eight hours with connections
  being broken, not for days. The failure of an I/O thread itself is made
  to happen by a test, in the simulator and on Linux 6.8 and 6.14. It has
  not been seen to happen by itself, and was not part of that run.
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
- `put`, `head` and `get` have been run against one real store, Linode
  Object Storage, three times from AArch64 and x86-64
  (`m24-put-head-get-linode.md`). It honours
  `If-None-Match` and `If-Match` and serves ranges exactly. It reports
  every object as encrypted whether or not that was asked for, and it
  neither checks nor returns the SHA-256 or the CRC-64 sent with a write:
  a body sent with the SHA-256 of other bytes was stored. On such a store
  only reading the bytes back proves them. Amazon S3 has not been run.
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
