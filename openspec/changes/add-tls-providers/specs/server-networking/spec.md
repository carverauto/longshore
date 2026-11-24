## MODIFIED Requirements

### Requirement: TLS as a build feature
TLS SHALL be built only when a TLS provider feature (`aws-lc` or `ring`) is
on, with `aws-lc` on by default, and the `longshore` crate built with
neither SHALL NOT depend on `longshore_tls`, `rustls`, `aws-lc-sys` or
`aws-lc-rs`.

#### Scenario: Plain build
- **WHEN** the dependencies of `longshore` are listed with default features off, leaving out those used only by tests
- **THEN** the list holds none of `longshore_tls`, `rustls`, `aws-lc-sys` and `aws-lc-rs`

#### Scenario: TLS asked for in a plain build
- **WHEN** a client built without a provider feature is configured to use TLS
- **THEN** building the client fails with an error that names the feature

#### Scenario: Tests without the feature
- **WHEN** the workspace's tests run with no provider feature on
- **THEN** the tests that need no TLS pass
