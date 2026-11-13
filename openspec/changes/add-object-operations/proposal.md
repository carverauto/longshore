## Why
The store covers store, conditional store, fetch, list and erase. Callers of
a general object storage library also expect to read an object's metadata,
set its content type and metadata, copy it on the server, delete many keys in
one request, list one level of a hierarchy, and hand out time-limited URLs.

## What Changes
- `stat`: size, checksum, ETag and last-modified time of an object without
  fetching it
- Request options on store and multipart begin: content type, user metadata,
  storage class and server-side encryption headers, held in a fixed-size value
- `copy`: server-side copy from one key to another
- `erase_many`: up to 1000 keys in one request
- Listing with a delimiter, returning common prefixes
- Presigned URLs for fetch and store, computed without I/O

## Impact
- Affected specs: `object-operations` (new)
- Affected code: `store::s3` (new operation kinds), `store::list` (common
  prefixes), `store::sigv4` (query-string signing), `http::head` (response
  header access for `stat`)
- First principles: none of these carries a body through the client except
  the key list of `erase_many`, which the caller writes into a registered
  buffer. Presigning is pure computation into a caller buffer.
