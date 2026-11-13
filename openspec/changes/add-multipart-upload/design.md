## Context
The store has fixed operation slots. Each slot holds the encoded key, the
body buffer and retry state, and nothing is allocated per request. Multipart
needs an upload id carried across requests and an ETag returned per part.

## Goals / Non-Goals
- Goals: uploads of any size; parts in parallel on separate connections; the
  same retry and checksum behaviour as a whole-object store
- Non-Goals: the store does not track an upload's parts. The caller owns the
  part list. A stream interface on top belongs to the high-level client.

## Decisions
- The caller holds upload state. `UploadId` and `PartTag` are fixed-size
  `Copy` values the caller keeps and passes back. The store keeps no table of
  open uploads, so there is nothing to size and nothing to leak.
  Alternative considered: an upload table inside the store. Rejected because
  it adds a second capacity limit and a cleanup path for abandoned entries.
- `UploadId` holds up to 256 bytes and `PartTag` up to 64. A longer value from
  a store fails the operation with a distinct error instead of being cut.
- The completion document is written by the caller into a registered buffer
  with `multipart::write_manifest`, then sent like any body. This keeps the
  store free of a buffer of its own.
- A part is retried like a whole-object store: the same bytes and part number
  are sent again, which the protocol defines as a replacement.
- Completion is retried only on transport failures and retryable statuses. A
  completion that reports the upload no longer exists after an earlier
  attempt was sent is reported as `Stored { after_retry: true }` only if a
  HEAD of the key returns the expected whole-object checksum; otherwise it is
  an error. This mirrors the conditional-store comparison that already exists.
- Abort is idempotent and retried freely. A missing upload counts as aborted.

## Risks / Trade-offs
- A caller that drops its `UploadId` leaves parts stored and billed. The
  documentation says to set a bucket lifecycle rule for incomplete uploads,
  and the high-level client aborts on drop.
- Stores differ in whether they accept a per-part CRC-64/NVME. The checksum
  mode stays a per-call choice, and the real-store runs record what each
  store accepts.

## Open Questions
- Whether Google Cloud Storage's XML multipart accepts CRC-32C per part in the
  same header it uses for whole objects. To be settled by a run against a
  real bucket.
