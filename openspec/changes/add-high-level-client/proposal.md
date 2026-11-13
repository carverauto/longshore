## Why
Using the library today means owning the event loop, registering buffers,
submitting operations with tokens and polling for outcomes. That suits a
storage engine and is a barrier for everyone else. Other projects need a
client they can build and call.

## What Changes
- A `Client` that starts and owns the I/O thread
- Async methods that work under any executor, and blocking equivalents
- A copying path that takes and returns byte slices, and a lending path that
  hands the caller a registered buffer so the body is never copied
- Streaming upload built on multipart and streaming download built on ranged
  fetches
- Runnable examples, and a benchmark of what the layer costs over the
  low-level API
- The crate compiles on platforms other than Linux, where building a client
  returns an unsupported error, so documentation builds

## Impact
- Affected specs: `high-level-client` (new)
- Affected code: a new `client` module; no change to the low-level API
- Depends on: `add-multipart-upload` for streaming upload, and
  `add-object-operations` for `stat`
- First principles: the I/O thread is unchanged. The caller's thread and the
  I/O thread exchange fixed-size messages through bounded queues that are
  allocated when the client is built. The lending path adds no copy. The
  copying path adds exactly one, into or out of a registered buffer, and is
  documented as the convenience path.
