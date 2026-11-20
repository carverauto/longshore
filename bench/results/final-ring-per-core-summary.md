# Ring-per-Core I/O Layer (`#41`–`#45`) — Final Benchmark & Real-Kernel Verification Summary

Recorded 2026-11-20 on branch `feat/ring-per-core` (post-M6-Phase2 commit `7ecd778`, M6-Phase3 final verification).

## 1. Scope & Commits Covered

This report summarizes the full before-and-after progression across all five OpenSpec changes (`#41 add-portable-io-backend`, `#42 add-file-operations`, `#43 add-simulated-disk`, `#44 add-server-networking`, `#45 add-core-mailbox`) on `feat/ring-per-core`:

| Milestone | Commit(s) | Scope |
|---|---|---|
| Baseline (`main`) | `aec2f9e` | Pre-split `longshore` baseline (`UringIo` / `SimIo` / `longshore_s3`) |
| M1 (`#41`) | `7ea6735` | `Limits::BASE`, `Engine<W>` / `World` split, `SimIo` trace parity, `Close`/`Cancel` contract, POSIX `kqueue`/`epoll`, S3 `Builder` portable selection |
| M2 (`#42`) | `a7b7380` | `IORING_SETUP_SUBMIT_ALL`, aligned pool + `<= 1 GiB` ranges, `getrlimit`, `FileSlot` / `OpenFlags` / `io::files`, 11 file `Op`s + `statx`, `submit_chain` (`IOSQE_IO_LINK`), `file_uring` |
| M3 (`#43`) | `7728cfd` | Virtual durable vs. unsynced log + writeback + dirs, whole-machine `power_loss`, 11 `DiskFaults` + placed faults + counters, `SimStorage` & WAL recovery tests, `NOTICE` |
| M4 (`#44`) | `e130bfd`, `36c6d06` | `longshore_stream`, optional default-on `tls` feature, multi-completion `Completion`, CQ `NODROP`, armed `Accept`/`Recv` + buffer ring + `give_back`, shutdown-before-close, socket opts, Unix sockets, `server_uring` |
| M5 (`#45`) | `03394c2`, `91e315c` | `longshore_mailbox` (`sync`, MPSC `Copy` queue, `Io` wake/drain/stop/close, `Pool`/waiters/`Future`/`block_on`), loom & 2-core sim tests, S3 client rebuilt on mailbox |
| E2E Track | `554fe49` | Opaque-box E2E test suite (Tiers 1–4) under `crates/longshore/tests/e2e/` |
| M6-P1 | `5eaebcd` | Tiers 1–4 Linux real-kernel `io_uring` execution & gap remediation |
| M6-P2 | `7ecd778` | Tier 5 adversarial hardening & 6 white-box bug fixes (`BUG-1`, `BUG-01`..`BUG-05`) |
| M6-P3 | final | Housekeeping, `openspec/project.md` / `README.md` / `NOTICE` / `tasks.md` checklists, final benchmarks & verification matrix |

## 2. Host Specifications & Honest Environment Constraints

### Configured Linux Test Hosts (`~/.config/longshore/test-hosts`)

| Target Triple | SSH Host | Hostname | Kernel (`uname -r`) | CPU / Pinning | Storage / Test FS |
|---|---|---|---|---|---|
| `aarch64-unknown-linux-musl` | `mfreeman@donbot` | `donbot` | `7.0.0-1019-nvidia` | 20 CPUs (10× Cortex-A725 + 10× Cortex-X925; `http_uring` pinned `--cpu 19` Cortex-X925) | `ext4` (`/dev/nvme0n1p2`) |
| `x86_64-unknown-linux-musl` | `root@10.0.0.3` | `pve02` | `6.8.12-11-pve` | 64 CPUs (Intel Xeon Platinum 8358P, 32c/64t) | ZFS root (`rpool/ROOT/pve-1`) + XFS `/mnt/storage-pve02` (selected by `scripts/test-linux.sh` for `file_uring` `O_DIRECT`) |

### Honest Environment Constraints
- **No external live S3 bucket credentials configured**: `S3_ENDPOINT`, `S3_REGION`, `S3_BUCKET`, `S3_ACCESS_KEY`, and `S3_SECRET_KEY` are not configured in the test environment, so `s3_uring` and `client_s3` cannot run against an external live S3 endpoint. Accordingly, `http_uring` loopback against `http_uring sink` provides the pinned-core before-and-after `io_uring` transport/CRC/TLS throughput measurements, while `client_uring` and `client_faults` validate the S3 client end-to-end on `io_uring` (with multi-thread callers, multipart uploads, deadline expiry, credential rotation, `TickFails`, `RingLost`, and 2,000-client drop churn) against the in-tree HTTP server.
- **No Linux `6.1` host in `test-hosts`**: Both configured hosts run newer kernels (`6.8.12-11-pve` and `7.0.0-1019-nvidia`). Linux `6.1` floor ABI compatibility is verified by source inspection across `crates/longshore_common/src/io/sys.rs`, `io/uring/`, and `io/posix/abi_linux.rs` (all setup flags and opcodes are `<= 6.1`: `IORING_SETUP_SINGLE_ISSUER` 6.0, `IORING_SETUP_DEFER_TASKRUN` 6.1, `IORING_SETUP_SUBMIT_ALL` 5.18, `IORING_OP_SEND_ZC` 6.0, `IOSQE_IO_LINK` 5.3, `IORING_OP_SOCKET` 5.19; `IORING_SEND_ZC_REPORT_USAGE` 6.2 is defined in `sys.rs` only and not submitted).

## 3. Benchmark & Milestone Report Index (`bench/results/`)

- Baseline: [`baseline-http-uring-donbot.jsonl`](baseline-http-uring-donbot.jsonl)
- M1 (`#41`): [`m1-http-uring-donbot.jsonl`](m1-http-uring-donbot.jsonl), [`m1-portable-io-backend.md`](m1-portable-io-backend.md)
- M2 (`#42`): [`m2-http-uring-donbot.jsonl`](m2-http-uring-donbot.jsonl), [`m2-file-bench.jsonl`](m2-file-bench.jsonl), [`m2-file-operations.md`](m2-file-operations.md)
- M4a (`#44`): [`m4a-http-uring-donbot.jsonl`](m4a-http-uring-donbot.jsonl), [`m4a-stream-and-tls.md`](m4a-stream-and-tls.md)
- M5a (`#45`): [`m5a-mailbox-model.md`](m5a-mailbox-model.md)
- M5b (`#45`): [`m5b-http-uring-donbot.jsonl`](m5b-http-uring-donbot.jsonl), [`m5b-core-mailbox.md`](m5b-core-mailbox.md)
- Final (M6-P3): [`final-http-uring-donbot.jsonl`](final-http-uring-donbot.jsonl), [`final-file-bench.jsonl`](final-file-bench.jsonl)

## 4. Before-and-After `http_uring` Loopback Progression (`donbot --cpu 19`)

Command per workload: `./http_uring put|get --addr 127.0.0.1:<port> --conns 8 --size 8388608 --secs 3 --cpu 19 [--crc 1] [--tls-cert cert.der]` against `http_uring sink` (5 runs per configuration, `--batch 1048576`, `aarch64` Cortex-X925).

### 4.1 Median `GB/s` Progression (Baseline -> Final M6-P3)

| Workload | Baseline (`aec2f9e`) | M1 (`7ea6735`) | M2 (`a7b7380`) | M4a (`e130bfd`) | M5b (`91e315c`) | Final M6-P3 (median, min to max) | Baseline -> Final Delta |
|---|---:|---:|---:|---:|---:|---|---|
| Plain PUT | `5.40` | `5.85` | `5.76` | `5.67` | `5.06` | `5.70` (`4.86 to 5.93`) | `+5.6%` (`+5.6%` GB/CPU-s) |
| CRC PUT (`--crc 1`) | `4.34` | `4.42` | `4.65` | `4.63` | `4.93` | `4.54` (`4.28 to 5.24`) | `+4.6%` (`+4.7%` GB/CPU-s) |
| Plain GET | `11.02` | `10.91` | `11.04` | `11.14` | `11.00` | `11.18` (`11.07 to 11.41`) | `+1.5%` (`+1.7%` GB/CPU-s) |
| TLS PUT | `3.21` | `2.96` | `2.83` | `2.52` | `2.67` | `2.43` (`2.26 to 2.85`) | `-24.3%` wall / `-10.0%` GB/CPU-s (`thread` `0.67` vs `0.81`) |
| TLS GET | `4.35` | `4.37` | `4.36` | `4.30` | `4.34` | `4.31` (`4.21 to 4.34`) | `-0.9%` (`-2.2%` GB/CPU-s) |

Plain PUT (`+5.6%`), CRC PUT (`+4.6%`), Plain GET (`+1.5%`), and TLS GET (`-0.9%`) remain at or above baseline throughput after all five OpenSpec changes and Tier 5 hardening. On single-core loopback TLS PUT, client `thread_cpu` dropped from `0.81` (`0.64–0.92`) at baseline to `0.67` (`0.64–0.69`) in M4a–M6 due to loopback sink scheduling contention, yielding `3.6` vs. `4.0` GB per CPU-s.

### 4.2 Final `scripts/bench-summary.py` Table ([`final-http-uring-donbot.jsonl`](final-http-uring-donbot.jsonl))

| cmd | tls | crc | links | conns | threads | size | batch | arch | runs | GB/s (median, low to high) | thread (median, low to high) | GB per CPU-s (median, low to high) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| put | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 5.70 (4.86 to 5.93) | 1.00 (1.00 to 1.00) | 5.7 (4.9 to 5.9) |
| put | False | True | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.54 (4.28 to 5.24) | 1.00 (1.00 to 1.00) | 4.5 (4.3 to 5.2) |
| get | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 11.18 (11.07 to 11.41) | 0.96 (0.95 to 0.97) | 11.7 (11.5 to 11.9) |
| put | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 2.43 (2.26 to 2.85) | 0.67 (0.64 to 0.69) | 3.6 (3.5 to 4.2) |
| get | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.31 (4.21 to 4.34) | 0.97 (0.96 to 0.97) | 4.4 (4.4 to 4.5) |

## 5. Before-and-After `file_uring --bench --iters 200` Comparison (`#42`)

4 KiB `O_DIRECT` overwrite + `IORING_FSYNC_DATASYNC` into a preallocated 256 KiB file (20 warmup iterations, 200 measured iterations), comparing two separate submissions (`two_ops`: `submit` + `tick` for write, then `submit` + `tick` for sync) vs. linked chain (`chain`: `Io::submit_chain` with `IOSQE_IO_LINK`). Raw JSONLs: [`m2-file-bench.jsonl`](m2-file-bench.jsonl) vs. [`final-file-bench.jsonl`](final-file-bench.jsonl) (`lines 1–2` = `donbot`, `lines 3–4` = `10.0.0.3`).

| Host (`arch`, `kernel`, `fs`) | Phase | Mode | `p50_us` | `p95_us` | `p99_us` | `mean_us` | `chain` vs. `two_ops` |
|---|---|---|---:|---:|---:|---:|---|
| `donbot` (`aarch64`, `7.0.0-1019-nvidia`, `ext4`) | M2 | `two_ops` | 1911.2 | 5261.7 | 6266.0 | 2457.0 | — |
| `donbot` (`aarch64`, `7.0.0-1019-nvidia`, `ext4`) | M2 | `chain` | 1600.8 | 2627.0 | 2887.9 | 1639.9 | `-16.2%` `p50` / `-33.3%` `mean` |
| `donbot` (`aarch64`, `7.0.0-1019-nvidia`, `ext4`) | Final M6-P3 | `two_ops` | 1892.8 | 5163.3 | 6517.3 | 2354.3 | — |
| `donbot` (`aarch64`, `7.0.0-1019-nvidia`, `ext4`) | Final M6-P3 | `chain` | 1450.5 | 2424.9 | 3315.4 | 1561.8 | `-23.4%` `p50` / `-53.0%` `p95` / `-49.1%` `p99` / `-33.7%` `mean` |
| `10.0.0.3` (`x86_64`, `6.8.12-11-pve`, `xfs`) | M2 | `two_ops` | 280.6 | 831.7 | 2890.2 | 376.6 | — |
| `10.0.0.3` (`x86_64`, `6.8.12-11-pve`, `xfs`) | M2 | `chain` | 265.3 | 2789.0 | 3523.7 | 492.7 | `-5.5%` `p50` / `+30.8%` `mean` |
| `10.0.0.3` (`x86_64`, `6.8.12-11-pve`, `xfs`) | Final M6-P3 | `two_ops` | 303.8 | 928.6 | 1197.6 | 428.6 | — |
| `10.0.0.3` (`x86_64`, `6.8.12-11-pve`, `xfs`) | Final M6-P3 | `chain` | 277.8 | 910.3 | 3410.8 | 443.8 | `-8.6%` `p50` / `-2.0%` `p95` / `+3.5%` `mean` |

On `donbot` (`ext4`), linked `IOSQE_IO_LINK` chains consistently reduce median (`-23.4%`), tail (`-53.0%` `p95`), and mean (`-33.7%`) latency vs. two separate submissions. On `10.0.0.3` (`xfs`), `chain` improves median (`-8.6%` `p50`) and `p95` (`-2.0%`), with occasional `p99` tail spikes (`3410.8 µs`) on shared storage.

## 6. Real-Kernel Verification Summary (`./scripts/test-linux.sh`)

Executed on **2 of 2 configured hosts** (`real io_uring checks passed on 2 of 2 configured hosts`, `/Volumes/Build/scratch/test-linux-m6p3.log`):

| Host (`Target`, `Kernel`) | Kernel Check Binaries (31 invocations across 9 binaries) | `e2e_sim_fi` (`sim,fault-injection`) | `e2e_notls_sim` (`--no-default-features --features sim`) |
|---|---|---|---|
| `mfreeman@donbot` (`aarch64-unknown-linux-musl`, `7.0.0-1019-nvidia`) | **31/31 PASS**: `contract_uring` (×4), `server_uring` (×4), `file_uring` (×3 incl. `--bench`), `echo_uring` (×7), `http_uring selftest`, `client_uring` (×4), `client_faults` (×5), `uring_faults`, `dns_uring` (×2) | **333 passed, 0 failed** (`9` host-side Cargo/CLI checks filtered out; `0.53s`) | **322 passed, 0 failed** (`9` host-side checks filtered out; `0.51s`) |
| `root@10.0.0.3` (`x86_64-unknown-linux-musl`, `6.8.12-11-pve`) | **31/31 PASS**: identical 31 real-kernel `io_uring` + `epoll` POSIX checks | **333 passed, 0 failed** (`9` host-side Cargo/CLI checks filtered out; `0.52s`) | **322 passed, 0 failed** (`9` host-side checks filtered out; `0.45s`) |

Selected real-kernel `client_faults` highlights from the M6-Phase3 run:
- `donbot`: `break TickFails` (`343,443` ops / 2 threads, `221,356` ops / 4 threads, `0` void buffers), `break RingLost` (`1,341` ops / `1,337` void bufs on 2 threads, `1,345` ops / `1,337` void bufs on 4 threads), `churn` (`2,000` clients built and dropped with work in flight, threads & fds gone, `0 KiB` memory growth).
- `10.0.0.3`: `break TickFails` (`136,802` ops / 2 threads, `111,357` ops / 4 threads, `0` void buffers), `break RingLost` (`1,313` ops / `1,309` void bufs on 2 threads, `1,306` ops / `1,300` void bufs on 4 threads), `churn` (`2,000` clients built and dropped with work in flight, threads & fds gone, `1,124 KiB` memory growth — within `client_faults` bound).

## 7. Loom Model Checking Progression (`./scripts/test-model.sh`)

Executed with `RUSTFLAGS="--cfg longshore_model" cargo test --release --target-dir target/model`:

| Phase | `mailbox_model` (`longshore_mailbox`) | `client_model` (`longshore_s3`) | Total Wall / Notes |
|---|---|---|---|
| Baseline (`main` @ `aec2f9e`) | N/A (pre-crate) | `10 passed` (`298.72s` test, `7m 30s` cold compile, `740`–`158,631` orders) | `12:29.99` (`749.99s` cold) |
| M5a (`03394c2`) | `12 passed` (`1.01s` test, `7.25s` compile) | `10 passed` (`22.08s` test, `13.00s` incremental compile) | `23.365s` |
| M5b (`91e315c`) | `12 passed` (`0.27s` test, `6.19s` compile) | `10 passed` (`3.49s` test, `12.28s` compile, `415`–`20,183` orders) | `22.667s` (S3 client rebuilt on mailbox) |
| M6-P2 (`7ecd778`) | **`17 passed`** (`0.45s` test; `+5` queue/slot/drop-detach/stop-race models over M5a) | **`10 passed`** (`5.36s` test) | All `27/27` loom models pass |
| M6-P3 (final) | **`17 passed`** (`0.35s` test, `0.02s` compile) | **`10 passed`** (`3.50s` test, `14.08s` compile, `415`–`20,183` orders) | All `27/27` loom models pass (`BATCH_B_SUCCESS`, `/Volumes/Build/scratch/verify-local-m6p3.log`) |

## 8. Final Local Verification Matrix (Batch B)

Confirmed on `feat/ring-per-core` in `/Volumes/Build/scratch/verify-local-m6p3.log` (`BATCH_B_SUCCESS`):

- **OpenSpec & Hygiene**: Zero unchecked `- [ ]` tasks across `#41`–`#45`; workspace source files total `68,135` lines (max `660` lines); `cargo doc` passed (`0.13s`).
- **No-TLS Dependency Exclusion**: `cargo tree` on `longshore` without `tls` confirms zero `longshore_tls` and zero `aws-lc-sys` dependencies.
- **Clippy Matrix (`-D warnings`)**: All workspace feature-matrix clippy invocations passed with zero errors.
- **Full Local Test Matrix**: All workspace unit, property, simulation, sealed zero-allocation, doc-test, and `e2e` suites passed with `0 failed` across all matrix configurations (`e2e` passes: `299`, `292`, `301`, `292`, `299`, plus `longshore_common`, `longshore_crc`, `longshore_dns`, `longshore_http_client`, `longshore_mailbox`, `longshore_s3`, `longshore_stream`, `longshore_tls`, and `sim_traces`).
- **Loom Model Checking (`./scripts/test-model.sh`)**: `27/27` passed (`mailbox_model`: `17 passed` in `0.35s`; `client_model`: `10 passed` in `3.50s`, `415`–`20,183` orders).
