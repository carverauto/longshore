## Context
`longshore_tls` splits TLS 1.3: rustls runs the handshake, then its traffic
keys move into `record::Direction`, which seals and opens records itself with
AWS-LC's scatter/gather calls (`bench/results/m6-aead.md`: the no-copy call is
11 to 34% faster than the same library with a copy). Only two places touch a
cryptographic library: `record.rs` and the provider chosen in `handshake.rs`.
`ring` 0.17 is already in every build through `longshore_s3`'s signing.

## Goals / Non-Goals
- Goals: the user picks AWS-LC, ring, rustls, several or none at build
  time; the default stays AWS-LC and unchanged; no allocation per record
  with AWS-LC or ring; records identical across providers.
- Non-Goals: other providers (stdx/crypto cannot be a registry
  dependency); ciphers other than the two AES-GCM suites; TLS 1.2.

## Decisions
- **Features are additive; the choice is made at run time.** Cargo unifies
  features across a dependency graph, so two crates asking for different
  providers end up with both. Instead of `compile_error!`, all compile and
  `Builder::tls_provider` picks one; the default is the first present of
  AWS-LC, ring and rustls. A
  build with `longshore_tls` but no provider is a `compile_error!` inside
  `longshore_tls`, which only a direct dependency on it can reach.
- **An enum, not a trait object, for longshore's own ciphers.** `Direction`
  holds `enum Cipher { AwsLc(..), Ring(..), Rustls(..) }` behind the
  provider features; a match per record costs nothing next to 16 KiB of
  AES-GCM.
- **rustls's record layer through its public cipher API.** At the end of
  the handshake the negotiated `Tls13CipherSuite::aead_alg` builds a
  `MessageEncrypter` and a `MessageDecrypter` from the extracted traffic
  keys (`Tls13AeadAlgorithm::encrypter`/`decrypter`). Sealing calls
  `encrypt` and copies the encoded record out; `encrypt` allocates a
  `Vec` per record, so the call, and dropping the two boxes, run inside
  `allow_alloc`, the exception the handshake already uses. Opening calls
  `decrypt` where the record lies (`open_in_place`) or on a copy of
  ciphertext and tag in the destination (`open`, which then needs 16 more
  bytes: `Direction::open_room`); neither allocates. rustls's keys cannot be
  copied, so a rustls direction is keyed to seal or to open, and only from
  the handshake's own key type.
- **Key wiping.** AWS-LC's context and ring's key are held inline and
  wiped with volatile writes on `clear`; rustls's boxes are dropped, and
  what rustls leaves in freed memory is outside longshore's control.
- **Ring seals in place.** `ring` has no call that reads from one buffer and
  writes another, so the body is copied into the record buffer, then
  `seal_in_place_separate_tag`; opening uses `open_in_place` on the record
  and copies the plaintext out (`open`) or leaves it (`open_in_place`).
- **The handshake provider follows the record provider.** A connection
  using ring or rustls records uses `rustls::crypto::ring`, so a ring-only
  or rustls-only build links no AWS-LC at all. The suite filtering in `handshake::crypto` applies to
  either provider.
- **`tls` keeps its meaning.** `tls = ["aws-lc"]`, so existing manifests
  are unchanged. The internal feature that turns the TLS code on is
  implied by any provider.

## Alternatives considered
- One provider at a time with `compile_error!` on both: breaks when two
  dependencies choose differently, which the user cannot fix.
- rustls `MessageEncrypter` as the only record path: simplest, but it
  allocates and copies twice per sealed record, so it is offered as a
  choice, not the default.
- stdx/crypto: git-only distribution makes it impossible for published
  crates; slower than AWS-LC on AArch64 in our measurements.

## Risks / Trade-offs
- The `rustls` provider allocates per sealed record → documented on the
  feature and the builder; the allocation guard's tests run it inside the
  exception; the default stays AWS-LC.
- Ring is slower than AWS-LC (m6: 7.5 against 8.3 GB/s on the X925, 5.8
  against 11.2 on Ice Lake, before the copy is counted) → AWS-LC stays the
  default; a new `bench/results/` entry states the cost per platform.
- More build configurations to lint and test → CI adds ring-only and
  rustls-only lint and test runs; `--all-features` covers all three
  together.

## Migration Plan
None for existing users. Ring-only users write
`default-features = false, features = ["ring"]`, rustls users
`features = ["rustls"]`; plain users keep
`default-features = false`.

## Open Questions
- None.
