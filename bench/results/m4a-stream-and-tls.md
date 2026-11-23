# Milestone M4a (`#44 add-server-networking`, Tasks 1.1–1.7, 2.1–2.3, 3.1–3.4) — Real-Kernel Verification & Benchmarks

Recorded 2026-11-19 on branch `feat/ring-per-core`.

## Real-Kernel Verification (`scripts/test-linux.sh`)

Executed `unset CARGO_TARGET_DIR && ./scripts/test-linux.sh` against both configured Linux test hosts:

| Target Triple | Host | Kernel (`uname -r`) | Checks Run | Result |
|---|---|---|---|---|
| `aarch64-unknown-linux-musl` | `mfreeman@donbot` | `7.0.0-1019-nvidia` | `contract_uring` (4 modes), `file_uring` (`UringIo` & `--posix` + `--bench`), `echo_uring` (7 modes), `http_uring selftest`, `client_uring` (4 modes), `client_faults` (5 modes), `dns_uring` (2 modes) | **Passed** |
| `x86_64-unknown-linux-musl` | `root@10.0.0.3` (`pve02`) | `6.8.12-11-pve` | Identical 26 real-kernel check invocations (`io_uring` + `epoll` POSIX backend) | **Passed** |

## S3 / HTTP `io_uring` Before-and-After Comparison (Task 1.7)

Loopback throughput benchmark on `donbot` (`aarch64`, Cortex-X925 pinned `--cpu 19`, 5 runs × 3 s, `8 MiB` objects, `--conns 8`, `--batch 1048576`) against `http_uring sink` (raw JSONL in `bench/results/m2-http-uring-donbot.jsonl` and `bench/results/m4a-http-uring-donbot.jsonl`). Note: `s3_uring` has no built-in loopback sink and targets an external S3 endpoint, so `http_uring` loopback exercises the extracted `longshore_stream` plain and TLS record/handshake paths on `io_uring`.

### Before (`m2`) vs. After (`m4a`) Comparison

| Workload | M2 Baseline Median (min–max) | Post-M4a Median (min–max) | M2 GB/CPU-s | Post-M4a GB/CPU-s | Delta (Median) |
|---|---|---|---|---|---|
| Plain PUT | `5.76 GB/s` (`4.83 to 6.41`) | `5.67 GB/s` (`4.77 to 6.59`) | `5.8` (`4.8 to 6.4`) | `5.7` (`4.8 to 6.6`) | `-1.6%` (`-1.7%` GB/CPU-s) |
| CRC PUT (`--crc 1`) | `4.65 GB/s` (`4.15 to 5.33`) | `4.63 GB/s` (`4.21 to 5.07`) | `4.7` (`4.2 to 5.3`) | `4.6` (`4.2 to 5.1`) | `-0.4%` (`-2.1%` GB/CPU-s) |
| Plain GET | `11.04 GB/s` (`10.83 to 11.10`) | `11.14 GB/s` (`11.06 to 11.30`) | `11.6` (`11.4 to 11.7`) | `11.7` (`11.5 to 11.8`) | `+0.9%` (`+0.9%` GB/CPU-s) |
| TLS PUT | `2.83 GB/s` (`2.20 to 3.15`) | `2.52 GB/s` (`2.19 to 2.88`) | `3.6` (`3.4 to 4.3`) | `3.7` (`3.5 to 4.1`) | `-11.0%` wall / `+2.8%` GB/CPU-s |
| TLS GET | `4.36 GB/s` (`4.29 to 4.36`) | `4.30 GB/s` (`4.22 to 4.32`) | `4.5` (`4.4 to 4.5`) | `4.4` (`4.4 to 4.5`) | `-1.4%` (`-2.2%` GB/CPU-s) |

### `scripts/bench-summary.py` Output (`bench/results/m4a-http-uring-donbot.jsonl`)

| cmd | tls | crc | links | conns | threads | size | batch | arch | runs | GB/s (median, low to high) | thread (median, low to high) | GB per CPU-s (median, low to high) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| put | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 5.67 (4.77 to 6.59) | 1.00 (1.00 to 1.00) | 5.7 (4.8 to 6.6) |
| put | False | True | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.63 (4.21 to 5.07) | 1.00 (1.00 to 1.00) | 4.6 (4.2 to 5.1) |
| get | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 11.14 (11.06 to 11.30) | 0.95 (0.95 to 0.96) | 11.7 (11.5 to 11.8) |
| put | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 2.52 (2.19 to 2.88) | 0.68 (0.64 to 0.72) | 3.7 (3.5 to 4.1) |
| get | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.30 (4.22 to 4.32) | 0.97 (0.96 to 0.97) | 4.4 (4.4 to 4.5) |
