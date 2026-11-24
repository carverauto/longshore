## 1. Features
- [ ] 1.1 `aws-lc`, `ring` and `rustls` features in `longshore_tls`, `longshore_stream`, `longshore_http_client`, `longshore_s3` and `longshore`; `tls = ["aws-lc"]`; `default = ["aws-lc"]`; `compile_error!` in `longshore_tls` when none is on
- [ ] 1.2 `rustls` crate features follow: `aws_lc_rs` with `aws-lc`, `ring` with `ring` and `rustls`; `aws-lc-sys` optional behind `aws-lc`; test certificates from rcgen's ring backend

## 2. Record layer
- [ ] 2.1 `record::Direction` holds `Cipher::AwsLc`, `Cipher::Ring` or `Cipher::Rustls`; keys set with a provider; `Direction::open` takes `&mut [u8]`
- [ ] 2.2 Ring: seal by copying into the record buffer and `seal_in_place_separate_tag`; `open` and `open_in_place` through `open_in_place`
- [ ] 2.3 rustls: encrypter and decrypter from the negotiated suite's `aead_alg`; `encrypt` and dropping inside `allow_alloc`; `decrypt` in place
- [ ] 2.4 `wide_gcm` compiled and called only with `aws-lc`
- [ ] 2.5 Tests: cross-provider round trip and identical records (AES-128/256-GCM, 1 to 16,384 bytes), tampered records, the existing record and edge tests run against each provider

## 3. Handshake and client
- [ ] 3.1 `handshake::crypto` takes the provider; `ClientSetup` and `ServerSetup` constructors carry it
- [ ] 3.2 `TlsProvider` and `Builder::tls_provider` in `longshore_s3`, re-exported by `longshore`; default the first compiled in of AWS-LC, ring and rustls; `BuildError::Setting` naming the provider when it is not compiled in
- [ ] 3.3 Tests: handshake and interop per provider (every client provider against every server provider), builder choices and errors, `alloc_guard` over application data with each provider

## 4. CI and documentation
- [ ] 4.1 CI: ring-only and rustls-only clippy and tests, and a `cargo tree` check that those builds hold no `aws-lc-sys` or `aws-lc-rs`; the plain-build check also excludes `rustls`
- [ ] 4.2 README and crate docs: the four builds (default, ring, rustls, plain), the rustls provider's allocation and `Builder::tls_provider`; `docs/testing.md`, `openspec/project.md` tech stack
- [ ] 4.3 Fuzz target `tls_record` builds against either provider

## 5. Measurement
- [ ] 5.1 `bench/results/`: record sealing and opening per provider (criterion `aead`), and `http_uring` HTTPS PUT/GET per provider in the Linux container and on an x86-64 host
- [ ] 5.2 Real-store run with the ring provider (Ceph or RustFS container over HTTPS), recorded in `bench/results/`
