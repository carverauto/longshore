## ADDED Requirements

### Requirement: Object metadata without a fetch
The store SHALL provide a `stat` operation that returns an object's size,
checksum when the store reports one, ETag and last-modified time without
transferring the body.

#### Scenario: Existing object
- **WHEN** `stat` is called for a key that holds an 8 MiB object
- **THEN** the outcome carries a size of 8 MiB and no body bytes are received

#### Scenario: Missing object
- **WHEN** `stat` is called for a key that does not exist
- **THEN** the outcome is `NotFound`

### Requirement: Request options
Store operations SHALL accept a content type, user metadata, a storage class
and server-side encryption headers, held in a fixed-capacity value that is
signed with the request and adds no allocation.

#### Scenario: Content type and metadata round trip
- **WHEN** an object is stored with a content type and one metadata entry
- **THEN** `stat` on that key reports the same content type and metadata entry

#### Scenario: Too many options
- **WHEN** the options do not fit the fixed capacity
- **THEN** building the options fails before any request is sent

### Requirement: Server-side copy
The store SHALL copy an object from one key to another without the bytes
passing through the client, and SHALL treat an error document returned with
status 200 as a failure.

#### Scenario: Copy within a bucket
- **WHEN** `copy` is called from an existing key to a new key
- **THEN** a fetch of the new key returns the same bytes and no object body was sent or received by the copy

### Requirement: Batch erase
The store SHALL erase up to 1000 keys in one request and SHALL report which
keys failed.

#### Scenario: Mixed result
- **WHEN** a batch names three keys and the store refuses one of them
- **THEN** the result names that key with its error and reports the other two as erased

### Requirement: Hierarchical listing
A listing SHALL accept a delimiter and SHALL return the common prefixes the
store reports alongside the keys.

#### Scenario: One level
- **WHEN** keys `a/1`, `a/2` and `b/1` exist and a listing uses delimiter `/` with an empty prefix
- **THEN** the listing returns common prefixes `a/` and `b/` and no keys

### Requirement: Presigned URLs
The library SHALL produce a presigned URL for a fetch or a store of one key,
valid for a stated duration, written into a caller-provided buffer without
performing I/O or allocating.

#### Scenario: Presigned fetch
- **WHEN** a presigned fetch URL for an existing key is requested with a one-hour expiry
- **THEN** a plain HTTP GET of that URL with no other credentials returns the object until the hour has passed

#### Scenario: Expiry longer than the credentials
- **WHEN** the credentials in use are temporary and expire before the requested URL expiry
- **THEN** the request for a URL fails with an error that says so
