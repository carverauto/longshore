# An in-cluster S3 cache on longshore — evaluation and first design

Status: draft for discussion (2026-11-26). Nothing built, nothing measured
for the cache itself. Every latency figure for the cache below is a model
built from measured parts (sources named at each one), not a measurement.
Once the direction is agreed, this becomes one or more OpenSpec changes.

Working name: `longshore_cache` (the library) and `longshore-cached` (the
server).

## 1. Is it a good idea?

### What it buys

**A hit is two to three orders of magnitude faster than S3.** A small S3 GET
in region is about 25 ms at the median and 100 ms at the p99, with stalls
past a second (longshore milestone 14: a 4 KiB ranged fetch at a median
under 33 ms and a p99 under 66 ms, power-of-two buckets; T4 `doc/06`: about
30 ms base plus 20 ms a MiB; Durner, Leis and Neumann, PVLDB 16(11): a few
percent of GETs past 600 ms, some never answered). The same request and
response between two hosts on the fabric takes 20 µs at the median and
26 µs at the p99 on longshore's ring (milestone 30, 324 bytes, DGX Spark
pair, 200 GbE). A cache hit is that round trip plus a lookup, plus a drive
read if the block is on NVMe.

**One fetch serves the whole cluster.** An in-process cache (T4, foyer)
holds what one process read. A cluster tier pools memory and NVMe across
nodes and shares warmth: node B's first read of a block node A fetched is a
hit. For a fleet of N readers of the same data that is N times fewer
misses, which is where the latency win actually comes from (section 4: the
p99 moves only with the hit ratio).

**It absorbs what S3 throttles.** S3 serves about 5,500 GETs a second per
prefix and answers past that with 503 Slow Down, which the client backs off
from (milestone 9 saw one-second erases from exactly this). Hot keys become
memory hits instead.

**It cuts request cost**, not bandwidth cost: S3 charges $0.0004 per 1,000
GETs and nothing for in-region bytes. A hit is a request not made.

**Misses can get faster too.** A cache that owns the fetch can race
stragglers and single-flight identical reads. T4 measured both: racing the
footer GET cut its slowest from 538 to 273 ms on Parquet, racing at the
p90 cut a modelled p99 from 633 to 495 ms, and one fetch per block cut
42.8 GB of GETs to 12.2 GB when Vortex read without merging (`doc/06`,
decisions 2, 8, 10).

### What it does not buy, said plainly

1. **The p99 improves only if the miss ratio is below about 1%.** Hits are
   fast and misses are S3; the mixture's p99 lies in the miss distribution
   whenever more than 1% of reads miss. At a 95% hit ratio the p99 is
   roughly S3's p80 (section 4). A workload that reads most bytes once
   (ETL, a single cold scan, backup restore) gets nothing but an extra hop.
2. **Writes are not faster.** A PUT is durable when S3 says so. A cache that
   acknowledges before S3 (write-back) trades S3's durability for a node's,
   which no object-store user expects. Writes go through; what the cache
   adds is that a read after the write is a hit.
3. **Consistency is the hard part, not speed.** S3 has been strongly
   read-after-write consistent since December 2020. A cache returns stale
   bytes the moment someone writes the object without going through it,
   unless every hit asks S3 first, which costs the S3 round trip the cache
   exists to remove. So the cache is correct without asking only for data
   that does not change in place (data-lake files, content-addressed or
   versioned keys, model weights, build artefacts) or that is written only
   through the cache. Section 3.6 makes this per prefix and explicit.
4. **"Transparent" has a security cost.** SigV4 is an HMAC: to verify a
   client's signature the cache needs that client's secret. The cache
   cannot re-check a request against AWS IAM without asking AWS. So the
   cache is its own trust domain: it issues its own access keys, verifies
   SigV4 itself, and serves only the buckets and prefixes each key is
   allowed (section 3.8). It fetches from S3 with its own role. This is how
   every S3-compatible proxy works, and it must be stated, because a cache
   that serves anything to anyone is a confused deputy.
5. **A cache is a stateful fleet** to run, size, upgrade and pay for
   (memory and NVMe). It must never be less available than S3: the client
   falls back to S3 directly when a cache node is slow or gone (3.7).
6. **Inside AWS, one TCP flow is capped at 5 Gbit/s** outside a cluster
   placement group (T4 `doc/05`). A 1 MiB hit over one connection takes
   about 1.7 ms however fast the cache is. Large hits want several
   connections, ENA Express, a placement group, or bare-metal fabric.
7. **Cross-AZ traffic costs $0.01 a GB each way** and adds 0.3 to 1 ms. The
   cache belongs in the readers' AZ; a cluster that spans AZs runs a cache
   per AZ.

### Against the alternatives

| Option | Hit p50 | What it is |
|---|---|---|
| S3 Standard | ~25 ms (small GET) | the baseline |
| S3 Express One Zone | single-digit ms (AWS's claim; not measured here) | a different storage class: one AZ, its own API quirks, higher storage price, no cache to run |
| In-process cache (T4, foyer) | 0.09 ms memory, 0.8 ms NVMe under load (T4 `doc/04`) | fastest per hit, but per process: no shared warmth, capacity bound to one box |
| In-cluster cache, this design | ~0.05 ms fabric, ~0.2 ms AWS same AZ (model, section 4) | shared warmth, pooled capacity, any S3 client |
| Alluxio, JuiceFS, Varnish/nginx | ms range | general-purpose; a JVM or a generic HTTP proxy on the data path, file-system semantics or no S3 auth |

These stack: an in-process T4-style tier in front of the cluster tier in
front of S3 or S3 Express. The cluster tier is the one missing from
longshore's world.

### Verdict

**Yes, with stated preconditions.** It is worth building if the target
workloads (UltraGraphDB, analytics over immutable files, model and
artefact serving) re-read data across processes or nodes, keep the miss
ratio under a few percent at the p50 and under 1% for p99 goals, and read
data that is immutable or written through the cache. It is not worth
building as a general S3 accelerator for arbitrary mutable buckets; the
consistency rules would leave it either wrong or no faster.

The go/no-go check before building past a prototype: replay a real trace
of the target workload against a simulated cache (LRU/CLOCK, the sizes on
offer) and read off the hit ratio. Below 90%, stop.

Longshore is unusually well placed for this because most of the server
already exists: the ring per core, armed accept and receive with buffer
rings, zero-copy send, `O_DIRECT` file ops on registered aligned buffers,
the mailbox, an HTTP/1.1 object server over `Io` (the test server in
`longshore_http_client::server`), the SigV4 code, and the S3 client for
the miss path, all running in the simulator.

## 2. What T4 teaches

T4 (`../t4`, v0.3.2) is an in-process cache: an `object_store::ObjectStore`
over another, memory and NVMe, io_uring underneath. It is not a server and
not a cluster. Its value here is its storage design and its measurements.

### Adopt

- **Fixed 1 MiB blocks, fixed 1 MiB chunks** (`doc/03`, decisions 1-2). A
  block is bytes `[k MiB, (k+1) MiB)` of an object version; a chunk is a
  1 MiB slot in memory or on disk; a bitmap of free chunks. Nothing
  fragments, nothing merges, an eviction frees exactly one slot. A 1 MiB
  I/O already saturates a drive; a 1 MiB GET costs about what a small one
  does.
- **A new ETag is a new version; version ids are never reused** (decision
  5). A reader holding an old lookup can never be handed another object's
  bytes. Block key = 40-bit version id, 24-bit block number, one `u64`.
- **Exclusive tiers** (decision 8): a block is in memory or on disk, never
  both. One place to look, one entry to change.
- **Fetch with `If-Match`** on the ETag the cache holds (decision 11), so a
  change behind the cache is caught at the next miss.
- **A block in flight is fetched once** (`doc/06`, decision 2): the first
  reader claims it, others wait on the claim. In a cluster where each key
  has one owner (3.7) this needs no coordination between nodes.
- **The read policy's measured defaults** (`doc/06`, decisions 8, 10, 11):
  GETs of at most 4 MiB for normal reads; a file's last 2 MiB (Parquet and
  Vortex footers) fetched in its own GET with two racing requests; a GET
  raced once more when it runs past the p90 of recent GET times; bytes in
  flight capped at the NIC rate times an unloaded GET time (200 MB at
  50 Gbit/s and 32 ms), with waiting adjacent GETs merged up to 4 MiB.
  Racing more than two did no better.
- **Striping chunks over drives by number**, chunk `c` on drive `c mod n`
  (`doc/04`): one I/O worker kept three drives at 98% and twelve at 94% of
  their cap. Adjacent chunks land on different drives, so a multi-block read
  reads all of them at once.
- **Coherence, not durability, for the disk tier** (`doc/13`): a log
  appended in batches with no sync keeps the disk tier across a process
  restart (rolling upgrades stay warm); after a reboot the tier starts
  cold. The log never names a chunk before its block is written.
- **Placement policy separate from the mechanism** (`doc/03` decision 7,
  `doc/06`): admission, promotion and victims are a policy's; CLOCK per tier
  is the default.
- **The test that matters most:** a fuzz target that checks the cache
  against the store under it, writes behind its back included, and the
  index against the tiers after every operation (`fuzz/cache_equivalence`).

### Adapt

- **T4's index is two B-trees from the global allocator** (decision 6). Here
  nothing allocates after setup: the block table is a fixed-capacity
  open-addressing table sized from the chunk count (at most one entry per
  chunk), the path table a fixed-capacity table with keys in a fixed arena.
- **T4 caches at 1 MiB and calls small objects amplification** (README,
  limitation 1). A cluster cache will see many small objects. Keep every
  chunk 1 MiB, but pack objects under 64 KiB into log-structured 1 MiB
  segments, evicted whole, hot entries re-appended on eviction (the
  CacheLib/Kangaroo approach). One chunk allocator, no second slab class to
  rebalance.
- **T4's network path is the part it could not fix.** Its own transport was
  built and removed (`doc/05`): one thread received only 1.5-2 GiB/s, the
  kernel's copy and rustls's copy out of its decrypt buffer being most of
  it; it still copies each fetched byte twice and wanted "a transport that
  lands a fetched block in its chunk". Longshore already has that: the
  lending path fetches into a registered buffer. Here the chunk is the
  registered buffer.
- **T4 learned that thread hops cost.** Its shared mode spent a 4 KiB hot
  read's 20-40 µs on five thread wakeups and frames crossing one syscall at
  a time (`doc/11`, Cost). A ring per core that owns its connections and its
  shard has no hop on a hit.
- **Huge pages for the memory tier** (`doc/05`): T4 lost most of its fill
  rate to 17 million first-touch faults and the kernel zeroing them. Here
  the tier is mapped, touched and registered once at startup.

### Avoid

- An async runtime, thread pools, `Vec`s on the read path, and `bytes()`
  joining a body: all of them show in T4's profiles.
- "Trust the first answer" as the only consistency rule: right for a data
  lake inside one process, wrong as the default for a shared cache many
  writers can bypass.

## 3. Design

### 3.1 Shape

```text
 application                      application (any S3 client)
 longshore_s3, cache-aware           |
   | key -> (node, core) by           | DNS / one endpoint
   | rendezvous hash                  v
   |                           any core (SO_REUSEPORT), forwards by mailbox
   v                                  |
 +---------------------- cache node ---------------------------------+
 | core 0 ring       core 1 ring        ...      core n-1 ring          |
 |  conns            conns                         conns               |
 |  shard 0          shard 1                       shard n-1           |
 |  index, chunks    index, chunks                 index, chunks       |
 |  NVMe slices      NVMe slices                   NVMe slices          |
 |  S3 conns ------- S3 conns ------------------- S3 conns ---> S3      |
 +-------------------------------------------------------------------+
```

Each core is a shard: its own ring, its own client connections, its own
slice of the memory tier, its own region of every NVMe drive, its own index
and its own S3 connections. Nothing is shared between cores on the data
path, so nothing is locked (principle 1).

### 3.2 Contradictions, resolved rather than traded

| Wanted | Against | Resolved by |
|---|---|---|
| Any S3 client works | the longshore client should not pay an extra hop | Speak plain S3 on the wire. The longshore client additionally learns the shard map and sends each key to its owning node and core; other clients land on any core, which forwards by mailbox (one hop, a few µs). |
| No locks, shared-nothing cores | a connection may arrive on the wrong core | the same: routing by the client, the mailbox for the rest |
| Strong consistency | not asking S3 on a hit | One owner per key. Writes through the cache go to the key's owner, which updates or forgets the entry when S3 acknowledges; that is linearisable per key without any coordination. Writes that bypass the cache are handled per prefix (3.6), and the cost is stated there. |
| Zero-copy hits | memory used for transfers vs for caching | The memory tier is the registered buffer pool: a hit is `SEND_ZC` straight from the chunk; a miss's S3 body lands in the chunk it will be cached in. |
| Small objects | fixed 1 MiB chunks | small objects in log-structured 1 MiB segments (2, Adapt) |
| A miss through the cache is a hop slower | a miss is S3's latency anyway | cut-through (bytes go to the client as they arrive) plus single-flight and hedging, which make the cache's p99.9 on misses lower than S3's direct (section 4) |
| Authentication on every request | microseconds per request | SigV4 signs the head only (principle 4): with the derived signing key cached per access key and day, verification is one SHA-256 of the canonical request and one HMAC, a few µs |

| Encrypted hits | zero-copy from the chunk | encrypt once at fill, so the chunk holds ciphertext; or let the network (Nitro, IPsec offload) encrypt (3.8) |

What survives as a real cost: writes that bypass the cache (3.6), and TLS
for generic HTTPS clients that use neither the network's encryption nor a
sidecar (3.8).

### 3.3 A hit

1. Armed receive from the buffer ring (later zero-copy receive, change
   `add-tcp-zero-copy-receive`); parse the request head in place.
2. Verify SigV4 and the key's permission for the bucket and prefix.
3. Look up path -> version, then (version, block) -> chunk.
4. Memory: `SEND_ZC` the response head and then the chunk's bytes, ranged
   requests sending only the bytes asked. The chunk's hold count keeps it
   from eviction until the send's notification.
5. NVMe: one `O_DIRECT` read per block into a staging chunk from the
   aligned pool, or, if the policy promotes, into a memory chunk, then send.
   Reads of several blocks go to several drives at once.

No allocation, no copy of the body, no thread hop when the client routed
correctly.

### 3.4 A miss

1. The owner claims the block(s); concurrent readers of the same blocks wait
   on the claim (single-flight, per shard, no cross-node protocol).
2. The read policy plans the GETs (T4's defaults: block-aligned, up to
   4 MiB, footers raced, stragglers raced at the p90, bytes in flight capped,
   adjacent waiting GETs merged).
3. The S3 client on the same ring fetches with `If-Match` into the chunks
   the blocks will live in. As each piece lands it is sent on to the client
   (cut-through), so the client sees S3's first byte plus one fabric hop.
4. A block is entered in the index once its last byte is in and its
   checksum (CRC-64/NVME on S3) verifies. A short or failed fetch caches
   nothing.
5. `412 Precondition Failed` means the object changed: forget the path,
   re-resolve, refetch.

Metadata: HEAD is answered from the path table when the prefix allows it
(3.6). LIST goes to S3, but the cache keeps the ETags and sizes it saw, as
T4 does, so a reader that lists first sends no HEADs.

### 3.5 Storage

- **Memory tier:** one huge-page mapping per core, touched and registered
  with io_uring at startup (in `<= 1 GiB` ranges, as the aligned pool is
  today), divided into 1 MiB chunks.
- **NVMe tier:** each drive split into per-core regions, chunks striped over
  drives by number, `O_DIRECT`, each core's ring submitting to all drives.
  Admission to NVMe is filtered (a block seen once is not written to disk)
  to spare drive endurance on scans.
- **Small objects:** below 64 KiB, appended to the core's open 1 MiB segment;
  a segment is evicted whole, live hot entries re-appended.
- **Index:** fixed-capacity tables sized from the chunk count, allocated at
  startup. Path keys up to 1,024 bytes in a fixed arena.
- **Eviction:** at insertion, in batches, as T4. CLOCK per tier as the
  first default; S3-FIFO is the candidate to measure against it (it filters
  one-hit wonders, which suits an object cache in front of scans).
- **Persistence:** T4's log (`doc/13`): coherence across a process restart,
  cold after a reboot, no sync.

### 3.6 Consistency, per prefix

Each bucket or prefix is configured with one mode. The modes say what the
cache promises; nothing is implicit.

| Mode | A hit asks S3 | Correct when | Latency gain |
|---|---|---|---|
| `immutable` | never | objects are never rewritten in place (data lake files, content-addressed, versioned keys) | full |
| `through` | never | every writer goes through the cache; bypassing writes are seen only at the next miss's `If-Match` or by S3 event notifications (seconds) | full |
| `ttl(d)` | after `d` since the entry was validated | bounded staleness is acceptable | full within `d` |
| `validate` | every time, conditional GET | strong consistency against any writer | none on latency; saves bandwidth only |

`through` is what makes the cache safe for UltraGraphDB-style owners that
are the only writer of their keys. `validate` exists so that the choice is
explicit; it is not why one builds this.

### 3.7 The cluster

- **Ownership:** rendezvous hashing of (bucket, key) over (node, core)
  shards; one owner per key, so single-flight and write ordering are local.
- **Membership:** a static list or a Kubernetes headless service, read at
  start and on change. A change moves only the keys whose owner changed;
  they refill from S3. A cache may lose anything.
- **Hot keys:** a key whose rate passes a threshold is replicated to its
  next shard in rendezvous order; the client spreads reads over both and can
  race them for a p99.9 goal.
- **Failure:** the longshore client gives a cache request a deadline of a
  few times the hit p99 and then goes to S3 directly, with its own
  credentials. So a dead or slow node costs one deadline, never an outage.
  Generic clients get the same from a health-checked endpoint.
- **Placement:** one cache per AZ, in the readers' AZ; a placement group or
  ENA Express where available.

### 3.8 Security and TLS

- The cache issues access keys, verifies SigV4, and maps each key to the
  buckets and prefixes it may read or write. It fetches from S3 with its own
  role (later: per-tenant roles through STS, once
  `add-credential-providers` lands).
- **Encrypted hits at plain cost** (resolved with `invent`, 2026-11-26).
  Per-connection TLS on a hit costs 130-170 ms of CPU per GB against 26 ms
  for a plain zero-copy send (milestone 12), and sealing needs a buffer
  apart from the chunk. The contradiction: the bytes on the wire must be
  ciphertext, and they must be the chunk's bytes, unchanged. Resolution:
  **make the chunk's bytes ciphertext.** Encrypt once, when a block enters
  the cache, not once per hit per connection. Four cases, by condition:

  1. **The network already encrypts.** AWS Nitro encrypts traffic between
     supported instance types in hardware, in the same or a peered VPC, as
     long as it does not pass a load balancer or transit gateway; VPC
     Encryption Controls can enforce and report it. On bare metal,
     ConnectX-7 IPsec packet offload encrypts on the card as it transmits,
     which leaves `SEND_ZC` from plaintext memory intact. Hits are then plain
     zero-copy sends. Measured on the DGX Spark pair (milestone 33): with
     packet offload on the ConnectX-7, an HTTP PUT sends at 13.0 GB/s
     (the link) for 39 ms of CPU a GB, against 29 ms in the clear and
     130 ms over TLS; zero-copy bulk TCP keeps 98% of plain's rate. What
     this does not give: a
     per-application end-to-end guarantee, or a check that the server is
     who it says it is above the network layer.
  2. **End to end required, longshore client: sealed at fill.** The fill
     seals each block in place in its chunk, in 64 KiB AEAD segments, while
     the bytes are still in the CPU's cache from the CRC check. Key per fill:
     `HKDF(epoch key, version id, block, fill id)`. Fill ids are never
     reused, so a key never seals two different plaintexts; the segment
     index is the nonce. A hit sends the sealed segments it covers by
     `SEND_ZC`, unchanged. The connection does a TLS 1.3 handshake (rustls,
     as now) and takes a session key from its exporter. Request and response
     heads are sealed with that key, a few hundred bytes each. The response
     head carries the block keys and segment tags; the body follows raw on
     the same stream, its length given in the head. The client decrypts in
     place in the buffer the body landed in: the same cipher work it does
     for TLS today, without record headers in the body. Cost per byte
     served: 26 + 131/R ms per GB, R being reads per fill (20 at a 95% hit
     ratio: 33 ms, against 130-170). Hit latency adds nothing; a 1 MiB TLS
     hit's 0.13-0.15 ms is gone.
  3. **The writer already encrypts** (client-side encryption in a segmented
     AEAD format, which UltraGraphDB could adopt): the cache stores and
     sends the ciphertext as it is, never sealing and never holding
     plaintext or keys.
  4. **Generic S3 clients over HTTPS:** standard TLS 1.3 per hit, the cost
     as measured. Alternatively a longshore sidecar on the client host
     (plain HTTP on localhost, case 2 to the cache) at about 10-20 µs.

  New costs of case 2, stated: an observer can see that two responses
  carry the same block, since the ciphertext is identical, which TLS
  hides; a body recorded off the wire stays readable for as long as its
  epoch key exists, so epochs rotate (hourly, say), and blocks are
  re-sealed or evicted before the old key is erased (2 TB an hour is
  0.56 GB/s, about 0.07 of a core); the protocol is longshore's own, so it
  needs its own review and fuzzing. Gains: both tiers are encrypted at
  rest for free, so drives and memory dumps hold no plaintext and no
  dm-crypt is needed. Card TLS offload stays TLS 1.2 only (milestone 12)
  and is not needed.

### 3.9 Against longshore's principles

1. One ring per core, no locks, no runtime: yes, shards are per core.
2. No allocation after setup: chunks, tables, arenas, connection slots and
   claims are fixed at build time.
3. No copy of a body on the lending path: hits are `SEND_ZC` from the chunk;
   misses land in the chunk and are sealed there in place (3.8). Exception:
   standard TLS for generic HTTPS clients.
4. Bodies are never hashed: SigV4 verification is over the head;
   integrity is S3's CRC-64/NVME, already verified by the client.
5. Protocol code does no I/O: the server, the cache and the policies are
   written against `Io`, so the simulator runs the whole cluster.
6. Timers chosen by the client: TTLs, hedging delays and membership
   refresh are the cache's.
7. No data-path dependency without a benchmark: none planned.

### 3.10 What longshore already has, and what is new

| Exists | New |
|---|---|
| ring per core, armed accept/recv, buffer rings, `SEND_ZC`, `SO_REUSEPORT` | the cache server: S3 request handling over the in-tree HTTP head parser |
| `O_DIRECT` file ops on registered aligned buffers, `submit_chain` | the chunk tiers, the index tables, the log |
| mailbox for cross-core hand-off | shard routing and forwarding |
| SigV4 signing (`sigv4.rs`) | SigV4 verification and the key/permission table |
| S3 client with the lending path, retries, CRC-64/NVME | the read policy (split, race, cap, merge) on top of it |
| HTTP/1.1 object server over `Io` (test server) | cut-through responses, ranged responses from chunks |
| simulator with network and disk faults, `alloc_guard`, loom | the cache-equivalence fuzz target, a multi-node simulated cluster |

### 3.11 Testing

- The whole cluster, its clients and a simulated S3 on one `SimIo`, with
  network and disk faults and trace hashes, as every protocol path is now.
- A cache-equivalence fuzz target in T4's style: every read through the
  cache returns what S3 holds, under writes through and around it, per
  consistency mode.
- `alloc_guard` over the data path.
- Real S3 and real fabric runs recorded in `../../../bench/results` before tasks are
  marked done: the DGX pair for the fabric case, an in-region AWS pair for
  the AWS case.

## 4. Latency estimate against S3 on AWS

### Assumptions

- **S3 Standard, small GET (<= 64 KiB):** lognormal, p50 25 ms, p99 100 ms,
  plus 0.2% of requests stalled by 0.3-1.5 s (milestone 14; Durner et al.;
  T4 `doc/06`).
- **S3 Standard, 1 MiB GET:** p50 50 ms, p99 200 ms, same stalls (Durner's
  model: 50 ms for 1 MiB at the median; milestone 21: a 1 MiB PUT's median
  46 ms).
- **Miss through the cache:** S3 plus 0.15 ms, raced once at the p90 (T4's
  `RaceStragglers`), the race modelled as independent of the first request.
- **Hit, AWS same AZ:** 70% memory (p50 0.15 ms, p99 0.5 ms: VPC round trip
  and jitter dominate), 30% NVMe (p50 0.3 ms, p99 1.5 ms). A 1 MiB hit adds
  1.7 ms for one 5 Gbit/s flow.
- **Hit, bare-metal fabric (200 GbE, as the DGX pair):** memory p50 0.04 ms,
  p99 0.1 ms (milestone 30: 20 and 26 µs request/response), NVMe p50
  0.15 ms, p99 0.8 ms. A 1 MiB hit adds 0.08 ms.
- Warm keep-alive connections, plain HTTP inside the cluster.

Monte Carlo over 2 million requests: `s3-cache-model.py`
(`uv run --with numpy python openspec/notes/s3-cache/s3-cache-model.py`). Its inputs are to be
replaced by measured distributions as section 6's measurements come in.

### Small GET (<= 64 KiB), ms

| | p50 | p90 | p99 | p99.9 |
|---|---|---|---|---|
| S3 direct | 25 | 54 | 105 | 931 |
| miss through the cache (hedged) | 25 | 54 | 83 | 107 |
| hit only, AWS same AZ | 0.18 | 0.44 | 1.1 | 1.9 |
| hit only, bare-metal fabric | 0.05 | 0.20 | 0.56 | 1.0 |
| 80% hits, AWS | 0.22 | 25 | 66 | 89 |
| 90% hits, AWS | 0.19 | 2.8 | 54 | 83 |
| 95% hits, AWS | 0.18 | 0.59 | 41 | 76 |
| 99% hits, AWS | 0.18 | 0.46 | 5.5 | 53 |
| 99.9% hits, AWS | 0.18 | 0.44 | 1.1 | 3.9 |
| 95% hits, fabric | 0.05 | 0.29 | 41 | 76 |
| 99% hits, fabric | 0.05 | 0.22 | 5.2 | 54 |
| 99.9% hits, fabric | 0.05 | 0.21 | 0.58 | 7.4 |

### 1 MiB GET, ms

| | p50 | p90 | p99 | p99.9 |
|---|---|---|---|---|
| S3 direct | 50 | 108 | 210 | 987 |
| miss through the cache (hedged) | 50 | 108 | 165 | 214 |
| hit only, AWS (one 5 Gbit/s flow) | 1.9 | 2.1 | 2.8 | 3.7 |
| hit only, fabric | 0.13 | 0.29 | 0.64 | 1.1 |
| 90% hits, AWS | 1.9 | 4.6 | 108 | 165 |
| 95% hits, AWS | 1.9 | 2.3 | 83 | 151 |
| 99% hits, AWS | 1.9 | 2.2 | 4.9 | 110 |
| 99.9% hits, AWS | 1.9 | 2.1 | 2.8 | 8.4 |
| 99% hits, fabric | 0.13 | 0.30 | 2.3 | 108 |

### Reading it

- **p50:** with more than about 60% hits, the median is a hit: 25 ms becomes
  0.18 ms on AWS (about 140x) and 0.05 ms on fabric (about 500x). For 1 MiB
  on AWS the flow cap holds it to about 25x unless the hit is spread over
  connections or the flow is faster.
- **p90:** becomes a hit once more than about 90% of reads hit: 54 ms to
  under 1 ms from 95% hits.
- **p99:** stays in S3's range until the hit ratio passes 99%. At 95% hits it
  is 41 ms against 105 (2.5x, half of it from hedging); at 99%, 5.5 ms
  (19x); at 99.9%, 1.1 ms (95x).
- **p99.9:** hedging alone takes S3's 0.9 s stall tail to about 0.1 s,
  whatever the hit ratio. This is the cache's gain on misses, and the most
  optimistic number here: T4 saw slow GETs come in bursts, when racing adds
  load, so the race is less independent than modelled.
- **Against S3 Express One Zone** (single-digit ms by AWS's claim): a hit is
  still 20-100x faster at the median; Express removes the need for a cache
  only where single-digit ms is enough.

### What would make these wrong

- Real hit ratios: the whole table hinges on them. The trace replay in
  section 1 is the first thing to run.
- AWS same-AZ round trips vary by instance family and load; the 0.15 ms
  memory-hit median is an assumption to be measured with an in-region pair.
- NVMe tails under write load (eviction and fills competing with hits).
- Cross-AZ placement, which adds 0.3-1 ms to every hit.

## 5. Open questions

1. Which workloads are first: UltraGraphDB's tiles and log, analytics over
   Parquet/Vortex, or model weights? Each sets the object sizes, the
   consistency mode and the hit ratio.
2. Is `through` enough for the first version, with `immutable` beside it,
   leaving `ttl` and S3 event notifications for later?
3. Does the cache serve GCS as well (CRC-32C, bearer tokens) from the start,
   or S3 only?
4. Plain HTTP inside the cluster as the default: acceptable for the intended
   deployments?
5. Kubernetes as the first membership source, or a static list?
6. Answered (milestone 33): the Sparks' ConnectX-7 takes IPsec packet
   offload, with zero-copy sends intact; about 10 to 14 ms of sender CPU a
   GB above plain TCP, not yet profiled.

## 6. Next steps

1. Trace replay: a hit-ratio simulation of the target workload (go/no-go).
2. Measure the hit path's parts on the DGX pair and an in-region AWS pair:
   round trip, `SEND_ZC` of 4 KiB to 1 MiB from registered memory, an
   `O_DIRECT` 1 MiB read under load.
3. OpenSpec changes, in order: `add-cache-storage` (chunks, tiers, index,
   log, in the simulator), `add-cache-server` (S3 requests, SigV4
   verification, hits and cut-through misses), `add-cache-read-policy`
   (T4's split/race/cap/merge on the longshore client),
   `add-cache-cluster` (shard map, client routing, fallback, hot keys).
