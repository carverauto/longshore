## ADDED Requirements

### Requirement: Google Cloud Storage through the high-level client
The high-level client SHALL reach Google Cloud Storage when its
configuration names that vendor, with the operations it offers on S3.

#### Scenario: Store and fetch
- **WHEN** a client configured for Google Cloud Storage stores an object and fetches it
- **THEN** the bytes fetched are the bytes stored and the fetch is verified by CRC-32C

#### Scenario: Batch erase
- **WHEN** a client configured for Google Cloud Storage is asked to erase a list of keys
- **THEN** each key is erased by a request of its own and the keys that could not be erased are reported

### Requirement: Reported operations on Google Cloud Storage
The put, head and get that report what the store answered SHALL work on
Google Cloud Storage, with an object's generation in the place of a
version id.

#### Scenario: A put reports its generation
- **WHEN** an object is put
- **THEN** the report carries the generation the store gave it

#### Scenario: A get of a named generation
- **WHEN** a get names a generation
- **THEN** that generation is fetched, or the report says it does not exist

### Requirement: Bearer token from the metadata server
The store SHALL be able to obtain its bearer token from the Compute Engine
metadata server and replace it before it expires.

#### Scenario: First token
- **WHEN** the store starts with the metadata server as its token source
- **THEN** its first request carries a token the server returned

#### Scenario: Refresh
- **WHEN** a token nears its expiry
- **THEN** a new one is fetched and used, and no request is sent with an expired token

#### Scenario: Server unavailable
- **WHEN** the metadata server does not answer and the current token is still valid
- **THEN** requests go on with the current token and the fetch is tried again

### Requirement: Zonal buckets
A store configured as a zonal bucket SHALL refuse, when it is submitted,
an operation such a bucket does not accept.

#### Scenario: Multipart upload
- **WHEN** a multipart upload is begun on a store configured as zonal
- **THEN** it is refused with an error that says zonal buckets do not accept multipart uploads, and nothing is sent

#### Scenario: Whole-object operations
- **WHEN** an object is stored in one request, fetched, and erased on a store configured as zonal
- **THEN** each works as on a standard bucket
