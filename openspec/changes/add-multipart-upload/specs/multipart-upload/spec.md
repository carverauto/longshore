## ADDED Requirements

### Requirement: Multipart upload operations
The store SHALL provide operations to begin a multipart upload, store a part,
complete the upload and abort it, each submitted with a caller token and
collected with `poll` like every other operation.

#### Scenario: Upload larger than one buffer
- **WHEN** a caller begins an upload, stores three parts from three registered buffers and completes with the three part tags
- **THEN** the object at the key is the three parts in part-number order

#### Scenario: Parts in parallel and out of order
- **WHEN** parts are stored concurrently on separate connections and finish in a different order than their part numbers
- **THEN** completion with the tags listed in part-number order succeeds

#### Scenario: Abort
- **WHEN** a caller aborts an upload
- **THEN** the outcome is `Aborted` and no object exists at the key because of that upload

#### Scenario: Abort of an upload that is already gone
- **WHEN** the store reports that the upload does not exist
- **THEN** the outcome is `Aborted`

### Requirement: Parts follow the first principles
A part SHALL be sent from one caller-owned registered buffer without being
copied, and no multipart operation SHALL allocate after the client is built.

#### Scenario: Sealed heap
- **WHEN** a multipart upload runs to completion in the simulator with the allocation guard sealed
- **THEN** no allocation is attempted

#### Scenario: Oversized identifier
- **WHEN** a store returns an upload id or part tag longer than the fixed-size type holds
- **THEN** the operation fails with a distinct error and no value is truncated

### Requirement: Part and object integrity
The store SHALL send a CRC-64/NVME checksum with a part when the caller asks
for one, and SHALL send the whole-object checksum on completion when the
caller provides it.

#### Scenario: Damaged part
- **WHEN** a part's bytes are changed in transit and the store rejects the checksum
- **THEN** the part is sent again from the caller's buffer and the attempt count reflects it

### Requirement: Completion reports the real result
The store SHALL read the completion response body and SHALL treat an error
document as a failure even when the status is 200.

#### Scenario: Error document with status 200
- **WHEN** the store answers a completion with status 200 and an error document naming a retryable error
- **THEN** the completion is attempted again

#### Scenario: Completion lost and retried
- **WHEN** a completion is sent, its response is lost, and the retry is told the upload does not exist
- **THEN** the outcome is success only if the object at the key carries the expected whole-object checksum, and an error otherwise

### Requirement: Multipart on every supported store
Multipart upload SHALL work against Amazon S3, S3-compatible stores and
Google Cloud Storage.

#### Scenario: Google Cloud Storage
- **WHEN** the vendor is Google Cloud Storage and the caller uses a bearer token
- **THEN** a multipart upload completes with that vendor's request forms
