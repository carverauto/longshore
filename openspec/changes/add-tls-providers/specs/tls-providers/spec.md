## ADDED Requirements

### Requirement: Provider features
`longshore` and the crates beneath it SHALL offer two TLS provider
features, `aws-lc` and `ring`, with `aws-lc` on by default and `tls` turning
on `aws-lc`. Each provider feature SHALL bring both the record cipher and
the rustls handshake provider of the same library.

#### Scenario: Ring only
- **WHEN** the dependencies of `longshore` are listed with default features off and `ring` on, leaving out those used only by tests
- **THEN** the list holds `longshore_tls`, `rustls` and `ring`, and neither `aws-lc-sys` nor `aws-lc-rs`

#### Scenario: Default build unchanged
- **WHEN** `longshore` is built with its default features
- **THEN** TLS records are sealed and opened with AWS-LC and the handshake uses rustls's AWS-LC provider

### Requirement: Provider chosen by the client
When both providers are compiled in, the client builder SHALL let the
caller choose one, and SHALL use AWS-LC when none is chosen. Choosing a
provider that is not compiled in SHALL fail the build of the client with an
error that names the provider.

#### Scenario: Ring chosen with both compiled in
- **WHEN** a client is built with TLS on and ring chosen, in a build with both features
- **THEN** its connections seal and open records with ring and handshake with rustls's ring provider

#### Scenario: Default with only ring compiled in
- **WHEN** a client is built with TLS on and no provider chosen, in a build with only `ring`
- **THEN** it uses ring

#### Scenario: Provider not compiled in
- **WHEN** a client in a build without `aws-lc` is configured with AWS-LC chosen
- **THEN** building the client fails with an error that names `aws-lc`

### Requirement: Same records from either provider
Records sealed with one provider SHALL open with the other, and both SHALL
produce identical records for the same keys, sequence numbers and contents,
for AES-128-GCM and AES-256-GCM.

#### Scenario: Cross-provider round trip
- **WHEN** records of 1, 255, 256, 4,096 and 16,384 bytes are sealed with each provider
- **THEN** the sealed bytes are equal and each opens with the other provider to the same contents and type

#### Scenario: Tampered record
- **WHEN** a byte of a sealed record is changed before opening, with either provider
- **THEN** opening fails and no plaintext is reported

### Requirement: No allocation per record with either provider
Sealing and opening records SHALL NOT allocate, with either provider, once
the keys are set.

#### Scenario: Ring under the allocation guard
- **WHEN** a TLS connection using ring sends and receives application data under `alloc_guard` after the handshake
- **THEN** no allocation is reported
