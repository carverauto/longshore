## ADDED Requirements

### Requirement: Operations that report what the store answered
The store and the shareable client SHALL provide `put`, `head` and `get`
operations that end with a report of what happened and do not turn the
store's status into success or failure.

#### Scenario: Stored
- **WHEN** `put` is called and the store answers 200
- **THEN** the report is `Answered` with status 200 and the `ETag`, version id and checksums the response carried

#### Scenario: Refused because the object exists
- **WHEN** `put` with `no_clobber` is called for a key that holds an object
- **THEN** the report is `Answered` with status 412, the object is unchanged, and no further request is made to compare it

#### Scenario: A header the store did not send
- **WHEN** encryption is asked for and the store's answer carries no encryption header
- **THEN** the answer's encryption is absent

### Requirement: A limit of attempts and time per operation
Each such operation SHALL take the number of requests it may send and an
optional deadline on the monotonic clock, and SHALL send no request beyond
either.

#### Scenario: One attempt
- **WHEN** an operation limited to one attempt is answered 503, or its connection closes without an answer
- **THEN** exactly one request reaches the store and the operation ends

#### Scenario: Deadline already past
- **WHEN** an operation is started with a deadline that has passed
- **THEN** it ends as not sent and no connection is opened

#### Scenario: Deadline during a request
- **WHEN** the deadline passes while a request is waiting for its answer
- **THEN** the request is cancelled, the operation ends as unknown, and no further attempt is made

### Requirement: Not sent, answered and unknown are told apart
A report SHALL say whether nothing was sent, whether the store answered the
last request sent, or whether a request was sent and got no answer, and SHALL
count the requests sent and the ones left unanswered.

#### Scenario: Answer lost
- **WHEN** the store carries out a `put` and the connection closes before its answer arrives
- **THEN** the report is `Unknown` with one request sent and one unanswered

#### Scenario: Retry after a lost answer
- **WHEN** two attempts are allowed, the first answer is lost and the second request is refused with 412
- **THEN** the report is `Answered` with status 412 and counts one unanswered request

### Requirement: The result marks the end of physical work
The result of such an operation, and the buffer it borrowed, SHALL be
delivered only after the I/O thread has finished with the operation and the
kernel no longer uses the buffer.

#### Scenario: Cancelled by a deadline
- **WHEN** a deadline cancels a request in progress
- **THEN** the result arrives after the cancelled operations have completed, and the buffer can carry the next operation at once

#### Scenario: I/O thread failure
- **WHEN** an I/O thread fails while a request that was sent is in progress
- **THEN** the operation ends as unknown, not as closed

### Requirement: Reads bound to an identity
`head` and `get` SHALL accept an expected `ETag`, sent as `If-Match`, and a
version id, sent as `versionId`, and SHALL report the response's `ETag`,
version id, length and exact `Content-Range`.

#### Scenario: Another object at the key
- **WHEN** `get` is called with an `ETag` the object does not have
- **THEN** the report is `Answered` with status 412 and no object bytes are delivered

#### Scenario: A range cut short
- **WHEN** a range that runs past the end of the object is asked for
- **THEN** the answer's `Content-Range` is the range the store served

### Requirement: Encryption and SHA-256 options
`put` SHALL accept a request for SSE-S3 encryption and a SHA-256 of the body
supplied by the caller, sent in place of the CRC-64/NVME, without the client
hashing the body with SHA-256.

#### Scenario: Body does not match its SHA-256
- **WHEN** `put` is called with a SHA-256 that is not the body's and the store checks it
- **THEN** the report is `Answered` with status 400 and nothing is stored

### Requirement: Evidence of the request sent
A report SHALL carry, for the last request handed to the kernel, its method,
its date, the names of its signed headers and the SHA-256 of the canonical
request its signature covers, and SHALL carry no key, signature or session
token. It SHALL carry none when nothing was sent.

#### Scenario: Rebuilt from expected values
- **WHEN** the canonical request is written out from the method, path, query and headers the caller asked for and the date reported
- **THEN** its SHA-256 equals the one in the report

#### Scenario: Nothing sent
- **WHEN** an operation ends as not sent
- **THEN** the report has no request evidence
