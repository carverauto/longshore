# tls-providers Specification

## Purpose
Which library protects HTTPS: AWS-LC (the default), ring, or rustls's own
record layer, each a Cargo feature, chosen per client when several are
compiled in, or none for a plain build. Measurements:
`bench/results/m29-tls-providers.md`.
## Requirements
### Requirement: Provider features
`longshore` and the crates beneath it SHALL offer three TLS provider
features, `aws-lc`, `ring` and `rustls`, with `aws-lc` on by default and
`tls` turning on `aws-lc`. `aws-lc` and `ring` SHALL seal and open records
in longshore's record layer with that library and handshake with rustls's
provider for the same library; `rustls` SHALL seal and open records with
rustls's own record encryption and handshake with rustls's ring provider.

#### Scenario: Ring only
- **WHEN** the dependencies of `longshore` are listed with default features off and `ring` on, leaving out those used only by tests
- **THEN** the list holds `longshore_tls`, `rustls` and `ring`, and neither `aws-lc-sys` nor `aws-lc-rs`

#### Scenario: rustls only
- **WHEN** the dependencies of `longshore` are listed with default features off and `rustls` on, leaving out those used only by tests
- **THEN** the list holds `longshore_tls`, `rustls` and `ring`, and neither `aws-lc-sys` nor `aws-lc-rs`

#### Scenario: Default build unchanged
- **WHEN** `longshore` is built with its default features
- **THEN** TLS records are sealed and opened with AWS-LC and the handshake uses rustls's AWS-LC provider

### Requirement: Provider chosen by the client
When several providers are compiled in, the client builder SHALL let the
caller choose one, and SHALL use the first compiled in of AWS-LC, ring and
rustls when none is chosen. Choosing a
provider that is not compiled in SHALL fail the build of the client with an
error that names the provider.

#### Scenario: Ring chosen with several compiled in
- **WHEN** a client is built with TLS on and ring chosen, in a build with all three features
- **THEN** its connections seal and open records with ring and handshake with rustls's ring provider

#### Scenario: Default with only ring compiled in
- **WHEN** a client is built with TLS on and no provider chosen, in a build with only `ring`
- **THEN** it uses ring

#### Scenario: Provider not compiled in
- **WHEN** a client in a build without `aws-lc` is configured with AWS-LC chosen
- **THEN** building the client fails with an error that names `aws-lc`

### Requirement: Same records from either provider
Records sealed with one provider SHALL open with each other, and all SHALL
produce identical records for the same keys, sequence numbers and contents,
for AES-128-GCM and AES-256-GCM.

#### Scenario: Cross-provider round trip
- **WHEN** records of 1, 255, 256, 4,096 and 16,384 bytes are sealed with each provider
- **THEN** the sealed bytes are equal and each opens with every other provider to the same contents and type

#### Scenario: Tampered record
- **WHEN** a byte of a sealed record is changed before opening, with either provider
- **THEN** opening fails and no plaintext is reported

### Requirement: Allocation per record by provider
With `aws-lc` and `ring`, sealing and opening records SHALL NOT allocate
once the keys are set. With `rustls`, opening SHALL NOT allocate, and the
allocation rustls makes for each sealed record SHALL happen inside the
sanctioned allocation exception.

#### Scenario: Ring under the allocation guard
- **WHEN** a TLS connection using ring sends and receives application data under `alloc_guard` after the handshake
- **THEN** no allocation is reported

#### Scenario: rustls under the allocation guard
- **WHEN** a TLS connection using rustls sends and receives application data under `alloc_guard` after the handshake
- **THEN** the run completes without the guard aborting

