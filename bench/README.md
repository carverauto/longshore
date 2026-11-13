# Benchmarks

Two kinds, by what is being measured.

## Microbenchmarks (criterion)

In-process, CPU-bound code: parsers (`dns_codec`, `http_head`), checksums
(`checksums`) and AEAD (`aead`). Substitute the bench name below.

```sh
cargo bench -p longshore-bench --bench dns_codec
cargo bench -p longshore-bench --bench dns_codec -- --save-baseline main   # record
cargo bench -p longshore-bench --bench dns_codec -- --baseline main        # compare
```

To run on a Linux host that has no Rust toolchain, cross-compile and copy the
bench executable (cargo prints its path):

```sh
cargo bench -p longshore-bench --bench dns_codec --no-run --target aarch64-unknown-linux-musl
cargo bench -p longshore-bench --bench dns_codec --no-run --target x86_64-unknown-linux-musl
scp target/aarch64-unknown-linux-musl/release/deps/dns_codec-<hash> HOST:dns_codec_bench
ssh HOST 'taskset -c <fast-core> ./dns_codec_bench --bench'
```

Criterion 0.8 compiles a few lines of C (`alloca`) for every Unix target.
Linking still uses the bundled `rust-lld` from `.cargo/config.toml`; only the
C compile needs a cross compiler. On macOS those are GCC 15.2.0 from the
messense toolchains, which `cc` finds on `PATH` as `aarch64-linux-musl-gcc`
and `x86_64-linux-musl-gcc`:

```sh
brew install messense/macos-cross-toolchains/aarch64-unknown-linux-musl \
             messense/macos-cross-toolchains/x86_64-unknown-linux-musl
```

Pin to one core and prefer an idle machine; criterion reports noise but cannot
remove it.

## Network throughput (`tcp_raw`)

Raw io_uring TCP send and receive between two hosts, one pinned thread under
test. See the header of `crates/longshore-bench/src/bin/tcp_raw.rs` for usage and
`results/` for recorded runs. This is not a criterion benchmark: it needs a
peer on another machine and reports CPU cost per byte, not just time.

HTTP PUT and GET throughput uses `http_uring` the same way, against its own
`sink` peer; see the header of `crates/longshore-bench/src/bin/http_uring.rs`.
S3 against a real bucket uses `s3_uring`; see the header of
`crates/longshore-bench/src/bin/s3_uring.rs`.

## Repeating runs

Every program prints one JSON line per run. A single short run is a rough
guide; run each configuration several times and summarise:

```sh
for i in 1 2 3 4 5; do ./http_uring put --addr 192.0.2.2:29200 --conns 16; done > runs.jsonl
scripts/bench-summary.py runs.jsonl      # median, lowest and highest per configuration
```

## AES-GCM on Arm cores AWS-LC does not recognise

HTTPS throughput is bound by AES-GCM. AWS-LC has a faster 8-way routine for
AArch64 that it uses only on CPUs it recognises (Neoverse V1, V2, V3 and
Apple M). On another core with the same instructions, for example the
Cortex-X925, build with the capabilities fixed at compile time:

```sh
scripts/build-fast-aes-aarch64.sh --release -p longshore-bench --bin http_uring
# -> target/fast-aes/aarch64-unknown-linux-musl/release/http_uring
```

Such a binary crashes on a CPU without the AES, PMULL and SHA3 instructions,
so it is for known hardware only. It is not needed on x86-64 or on Graviton.
Measured effect: `results/m11-tls-fast-aes-aarch64-dgx-spark.md`.
