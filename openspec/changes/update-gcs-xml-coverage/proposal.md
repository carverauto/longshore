## Why
Google Cloud Storage is reached through its XML API (`Vendor::Gcs`), and
stores, fetches, erases, listings, copies and multipart uploads have run
against a real bucket. Several things that work on S3 still do not on
Google Cloud Storage:

- the high-level client builds every store as S3, so it cannot be pointed
  at Google Cloud Storage at all
- the operations that report what the store answered (`add-reported-operations`)
  refuse any vendor but S3
- a batch erase is refused, because the XML API has no such request
- a bearer token comes only from a file someone else keeps fresh

Google's zonal "Rapid" buckets add a case of their own: they accept
whole-object writes and reads through the XML API but refuse multipart and
resumable uploads, which is how longshore sends large objects.

## What Changes
- The high-level client takes the vendor from its configuration
- Reported put, head and get work on Google Cloud Storage, with an
  object's generation where S3 has a version id
- The high-level client's batch erase works on Google Cloud Storage by
  erasing the keys one request each over its connections
- A bearer token can be fetched from the Compute Engine metadata server
  and refreshed ahead of its expiry
- A store can be marked as a zonal bucket. A multipart upload to it is
  refused when it is submitted, with an error that says why, instead of
  failing at the store
- Whole-object store, fetch, ranged fetch, head, listing and erase are run
  against a real zonal bucket and the result recorded

Not in this change, with the reasons in `design.md`: the JSON API,
presigned URLs for Google Cloud Storage, and gRPC (appendable objects,
`BidiReadObject`, direct connectivity).

## Impact
- Affected specs: `gcs-store` (new)
- Affected code: `longshore_s3` (`client` configuration and threads,
  `s3::exact`, `s3::rotation`, `creds`, `Config`), the test server (object
  generations), `longshore-bench` (`client_s3`, `s3_uring` selftests)
- Depends on: `add-high-level-client` and `add-reported-operations` for
  the operations it extends; shares the provider state machine and the
  JSON field reader with `add-credential-providers`
- First principles: unchanged. The metadata server request goes through
  the same `Io` trait and connection pool and parses into a fixed buffer
