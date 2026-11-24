## Context
`longshore_tls` splits TLS 1.3: rustls runs the handshake, then its traffic
keys move into `record::Direction`, which seals and opens records itself with
AWS-LC's scatter/gather calls (`bench/results/m6-aead.md`: the no-copy call is
11 to 34% faster than the same library with a copy). Only two places touch a
cryptographic library: `record.rs` and the provider chosen in `handshake.rs`.
`ring` 0.17 is already in every build through `longshore_s3`'s signing.

## Goals / Non-Goals
- Goals: the user picks AWS-LC, ring, both or neither at build time; the
  default stays AWS-LC and unchanged; no allocation per record with either;
  records identical across providers.
- Non-Goals: other providers (stdx/crypto cannot be a registry dependency;
  rustls's own record encryption allocates a buffer per record); ciphers
  other than the two AES-GCM suites; TLS 1.2.

## Decisions
- **Features are additive; the choice is made at run time.** Cargo unifies
  features across a dependency graph, so two crates asking for different
  providers end up with both. Instead of `compile_error!`, both compile and
  `Builder::tls_provider` picks one; the default is AWS-LC when present. A
  build with `longshore_tls` but no provider is a `compile_error!` inside
  `longshore_tls`, which only a direct dependency on it can reach.
- **No trait object.** `Direction` holds
  `enum Cipher { AwsLc(EVP_AEAD_CTX), Ring(LessSafeKey) }` behind the
  provider features; a match per record costs nothing next to 16 KiB of
  AES-GCM.
- **Ring seals in place.** `ring` has no call that reads from one buffer and
  writes another, so the body is copied into the record buffer, then
  `seal_in_place_separate_tag`; opening uses `open_in_place` on the record
  and copies the plaintext out (`open`) or leaves it (`open_in_place`).
- **The handshake provider follows the record provider.** A connection
  using ring records uses `rustls::crypto::ring`, so a ring-only build links
  no AWS-LC at all. The suite filtering in `handshake::crypto` applies to
  either provider.
- **`tls` keeps its meaning.** `tls = ["aws-lc"]`, so existing manifests
  are unchanged. The internal feature that turns the TLS code on is
  implied by either provider.

## Alternatives considered
- One provider at a time with `compile_error!` on both: breaks when two
  dependencies choose differently, which the user cannot fix.
- rustls `MessageEncrypter` for records: any rustls provider would work,
  but `encrypt` returns a newly allocated `Vec` per record, against the
  rule that nothing allocates on the data path.
- stdx/crypto: git-only distribution makes it impossible for published
  crates; slower than AWS-LC on AArch64 in our measurements.

## Risks / Trade-offs
- Ring is slower than AWS-LC (m6: 7.5 against 8.3 GB/s on the X925, 5.8
  against 11.2 on Ice Lake, before the copy is counted) → AWS-LC stays the
  default; a new `bench/results/` entry states the cost per platform.
- Twice the build configurations to lint and test → CI adds one ring-only
  lint and test run; `--all-features` covers both providers together.

## Migration Plan
None for existing users. Ring-only users write
`default-features = false, features = ["ring"]`; plain users keep
`default-features = false`.

## Open Questions
- None.
