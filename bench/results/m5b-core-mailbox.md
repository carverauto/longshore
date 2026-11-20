# Milestone M5b (`#45 add-core-mailbox`, Feature 27) — Real-Kernel Checks, Benchmarks & Model Runtime

Recorded 2026-11-19 on branch `feat/ring-per-core`.

## 1. Real-Kernel Verification (`scripts/test-linux.sh`)

Executed `unset CARGO_TARGET_DIR && ./scripts/test-linux.sh` against both configured Linux test hosts (31 check invocations across 9 binaries per host):

| Target Triple | Host | Kernel (`uname -r`) | Checks Run | Result |
|---|---|---|---|---|
| `aarch64-unknown-linux-musl` | `mfreeman@donbot` | `7.0.0-1019-nvidia` | `contract_uring` (4 modes), `server_uring` (4 modes), `file_uring` (3 modes), `echo_uring` (7 modes), `http_uring selftest`, `client_uring` (4 modes), `client_faults` (5 modes), `uring_faults`, `dns_uring` (2 modes) | **Passed (31/31)** |
| `x86_64-unknown-linux-musl` | `root@10.0.0.3` (`pve02`) | `6.8.12-11-pve` | Identical 31 real-kernel check invocations (`io_uring` + `epoll` POSIX backend) | **Passed (31/31)** |

### S3 Client Real-Kernel Highlights (`client_uring` & `client_faults`)
- **`client_uring`** (default, `--threads 4 --v6`, `--path-style`, `--rotate`):
  - `donbot` & `10.0.0.3`: all 4 modes passed (`32 round trips of 1048576 bytes`, 3-part multipart upload, exact `NotSent(Deadline)` in ~300–303 ms, `rotate` credentials reload after 8 refused probes, `longshore-io-*` threads stopped on drop, >200 ops quiesced 50 times).
- **`client_faults`** (`break tick`, `break ring`, `break tick --threads 4`, `break ring --threads 4`, `churn`):
  - `donbot`: `TickFails` (2 & 4 threads: 238,444 / 238,237 ops ended closed, 0 void buffers), `RingLost` (2 & 4 threads: 1,342 / 1,343 ops ended closed, 1,338 / 1,337 void buffers), `churn` (2,000 clients built & dropped with work in flight, threads & fds gone, `0 KiB` memory growth).
  - `10.0.0.3`: `TickFails` (2 & 4 threads: 176,305 / 176,122 ops ended closed, 0 void buffers), `RingLost` (2 & 4 threads: 1,360 / 1,342 ops ended closed, 1,356 / 1,336 void buffers), `churn` (2,000 clients built & dropped with work in flight, threads & fds gone, `1932 KiB` memory growth — within `client_faults` bound).

## 2. `donbot` `http_uring --cpu 19` Before-and-After Comparison

Loopback throughput benchmark on `donbot` (`aarch64`, Cortex-X925 pinned `--cpu 19`, 5 runs × 3 s, `8 MiB` objects, `--conns 8`, `--batch 1048576`) against `http_uring sink` (raw JSONL in `bench/results/m4a-http-uring-donbot.jsonl` and `bench/results/m5b-http-uring-donbot.jsonl`; `s3_uring` / `client_s3` require external S3 credentials, so loopback `http_uring` is used for before/after throughput while `client_uring` + `client_faults` validate the S3 client on `io_uring`).

### Before (`m4a`) vs. After (`m5b`) Comparison

| Workload | M4a Median (min–max) | Post-M5b Median (min–max) | M4a GB/CPU-s | Post-M5b GB/CPU-s | Delta (Median) |
|---|---|---|---|---|---|
| Plain PUT | `5.67 GB/s` (`4.77 to 6.59`) | `5.06 GB/s` (`4.65 to 6.28`) | `5.7` (`4.8 to 6.6`) | `5.1` (`4.7 to 6.3`) | `-10.8%` (within `4.6–6.6` loopback variance) |
| CRC PUT (`--crc 1`) | `4.63 GB/s` (`4.21 to 5.07`) | `4.93 GB/s` (`4.29 to 5.16`) | `4.6` (`4.2 to 5.1`) | `4.9` (`4.3 to 5.2`) | `+6.5%` (`+6.5%` GB/CPU-s) |
| Plain GET | `11.14 GB/s` (`11.06 to 11.30`) | `11.00 GB/s` (`10.75 to 11.17`) | `11.7` (`11.5 to 11.8`) | `11.5` (`11.3 to 11.6`) | `-1.3%` (`-1.7%` GB/CPU-s) |
| TLS PUT | `2.52 GB/s` (`2.19 to 2.88`) | `2.67 GB/s` (`2.02 to 3.04`) | `3.7` (`3.5 to 4.1`) | `3.7` (`3.4 to 4.2`) | `+6.0%` wall (`0.0%` GB/CPU-s) |
| TLS GET | `4.30 GB/s` (`4.22 to 4.32`) | `4.34 GB/s` (`4.14 to 4.38`) | `4.4` (`4.4 to 4.5`) | `4.5` (`4.3 to 4.5`) | `+0.9%` (`+2.3%` GB/CPU-s) |

### `scripts/bench-summary.py` Output (`bench/results/m5b-http-uring-donbot.jsonl`)

| cmd | tls | crc | links | conns | threads | size | batch | arch | runs | GB/s (median, low to high) | thread (median, low to high) | GB per CPU-s (median, low to high) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| put | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 5.06 (4.65 to 6.28) | 1.00 (1.00 to 1.00) | 5.1 (4.7 to 6.3) |
| put | False | True | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.93 (4.29 to 5.16) | 1.00 (1.00 to 1.00) | 4.9 (4.3 to 5.2) |
| get | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 11.00 (10.75 to 11.17) | 0.96 (0.95 to 0.96) | 11.5 (11.3 to 11.6) |
| put | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 2.67 (2.02 to 3.04) | 0.68 (0.59 to 0.83) | 3.7 (3.4 to 4.2) |
| get | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.34 (4.14 to 4.38) | 0.97 (0.96 to 0.98) | 4.5 (4.3 to 4.5) |

## 3. `./scripts/test-model.sh` Runtime Comparison

Executed `unset CARGO_TARGET_DIR && time ./scripts/test-model.sh` (`--cfg longshore_model --release --target-dir target/model`):

| Phase | Baseline (`baseline-test-model.txt`, pre-M5a) | M5a (`m5a-mailbox-model.md`, pre-M5b) | Post-M5b (`./scripts/test-model.sh`) | Notes |
|---|---|---|---|---|
| `longshore_mailbox` (`mailbox_model`, 12 tests) | N/A | `1.01s` test (`7.25s` compile) | `0.27s` test (`6.19s` compile) | All 12 queue & slot/answer `loom` tests pass |
| `longshore_s3` (`client_model`, 10 tests) | `298.72s` test (`7m30s` cold compile) | `22.08s` test (`13.00s` compile) | `3.49s` test (`12.28s` compile) | Unedited `tests/client/model.rs` & `model/machine.rs` pass (`415`–`20,183` orders) |
| Total `./scripts/test-model.sh` wall time | `12:29.99` (`749.99s`, cold `target/model`) | `23.365s` (warm) | `22.667s` (`23.28s` user, `5.04s` sys) | Both suites pass (`12/12` + `10/10`) |
