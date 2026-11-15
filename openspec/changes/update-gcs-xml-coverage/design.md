## Context
What exists: `Vendor::Gcs` changes the header prefix, sends a bearer token
in place of a signature, uses CRC-32C in `x-goog-hash`, and
`x-goog-if-generation-match: 0` for a conditional store. Recorded runs
against a real bucket cover object operations and multipart uploads
(`bench/results/m8-gcs.md`, `m16-multipart.md`, `m17-object-operations.md`).

Zonal buckets, from Google's documentation (read 2026-11-15):
- reads work through the XML API, the JSON API and gRPC
- writes work through the XML API "except for XML multipart uploads", and
  through gRPC's `BidiWriteObject`; "You can't write to zonal buckets by
  using the JSON API"
- resumable uploads, object rewrites, compose, HMAC keys, versioning and
  MD5 hashes are unavailable; integrity is CRC-32C only
- the documentation does not say whether ranged GET, listing, head and
  erase work through the XML API on such a bucket

## Goals / Non-Goals
- Goals: every operation longshore offers on S3 either works on Google
  Cloud Storage through the XML API or is refused when submitted with an
  error that names the reason; what works on a zonal bucket is known from
  a recorded run, not from the documentation
- Non-Goals: the JSON API; presigned URLs; gRPC; objects on a zonal
  bucket larger than one request can carry

## Decisions
- Decision: stay on the XML API and do not add the JSON API. The JSON API
  offers three things the XML API lacks that touch longshore's operations:
  a batch request, object rewrite, and metadata patch. It cannot write to
  a zonal bucket at all. Adding it means a second request and response
  vocabulary and a JSON parser reading network bytes, for one operation
  longshore has (batch erase), which is served well enough by single
  erases over pooled connections.
  - Alternatives considered: JSON batch for erase (`multipart/mixed`
    bodies, at most 100 calls each, a new parser); JSON rewrite for large
    copies (no caller asks for it yet)
- Decision: batch erase on Google Cloud Storage lives in the high-level
  client, not the store. The store's `erase_many` is one request with one
  outcome; many requests with partial failure is the client's kind of
  work. The store keeps answering `Unsupported`.
- Decision: a generation stands where a version id does. Google Cloud
  Storage names a version of an object by `generation`, a decimal number,
  in the `x-goog-generation` response header and the `generation` query
  parameter. The reported operations carry it in the type they already
  have for version ids. A precondition on the stored object's tag stays
  `If-Match`, which the XML API honours.
- Decision: a zonal bucket is declared in the configuration. Nothing in a
  response says so before the first object is touched, and refusing a
  multipart upload when it is submitted needs to know at once.
  - Alternatives considered: learn it from `x-goog-storage-class: RAPID`
    on the first response (late, and absent on some responses)
- Decision: no presigned URLs for Google Cloud Storage. They need either
  HMAC keys (unavailable on zonal buckets, and forbidden by the
  organisation the first test ran in) or an RSA signature with a
  service-account key, which is a new cryptographic dependency.
- Decision: the metadata server is the one token source added. It is a
  plain HTTP GET on a link-local address that answers a small JSON object
  with `access_token` and `expires_in`. Exchanging a service-account key
  for a token needs an RSA signature and is left out for the same reason
  as presigning.

## Risks / Trade-offs
- The documentation is silent on several XML operations for zonal
  buckets → task 4.1 runs each against a real one before anything is
  claimed
- Large objects cannot be stored in a zonal bucket by this change: one
  request carries at most one buffer → stated in the error and the
  README; the way past it is gRPC's appendable write, which is its own
  change and needs HTTP/2
- Erasing key by key costs one request each → bounded by the pool; a
  listing-driven erase already works this way on Google Cloud Storage

## Migration Plan
`Config` gains a field, so every place that builds one changes. No
behaviour changes for S3.

## Open Questions
- Does a zonal bucket return `x-goog-hash` on fetch, and accept
  `x-goog-if-generation-match: 0`, as a standard bucket does?
- The size of the largest single PUT a zonal bucket accepts
