# Milestone M2 (`#42 add-file-operations`) — Real-Kernel Verification & Benchmarks

Recorded 2026-11-19 on branch `feat/ring-per-core`.

## Real-Kernel Verification (`scripts/test-linux.sh`)

Executed `unset CARGO_TARGET_DIR && ./scripts/test-linux.sh` against both configured Linux test hosts:

| Target Triple | Host | Kernel (`uname -r`) | File Test FS | Checks Run | Result |
|---|---|---|---|---|---|
| `aarch64-unknown-linux-musl` | `mfreeman@donbot` | `7.0.0-1019-nvidia` | `ext4` | `contract_uring` (4 modes), `file_uring` (`UringIo` & `--posix` + `--bench`), `echo_uring` (7 modes), `http_uring selftest`, `client_uring` (4 modes), `client_faults` (5 modes), `dns_uring` (2 modes) | **Passed** |
| `x86_64-unknown-linux-musl` | `root@10.0.0.3` (`pve02`) | `6.8.12-11-pve` | `xfs` (`/mnt/storage-pve02`) | Identical 26 real-kernel check invocations (`io_uring` + `epoll` POSIX backend) | **Passed** |

Note on `10.0.0.3` filesystem selection: `root@10.0.0.3` home directory sits on `zfs` (`rpool/ROOT/pve-1`), where `file_uring` hung in `iou-wrk` during `IORING_OP_WRITE_FIXED` in the `>16,384` buffers check on Linux `6.8.12-11-pve`. `scripts/test-linux.sh` selects `/mnt/storage-pve02` (`xfs`) when `.` is `zfs` or `tmpfs`.

Note on Linux 6.1 compatibility: No Linux 6.1 host is configured in `~/.config/longshore/test-hosts`. Static inspection of `crates/longshore_common/src/io/sys.rs`, `io/uring/setup.rs`, `io/uring/ops.rs`, and `io/posix/abi_linux.rs` confirms all setup flags and opcodes are `<= 6.1` (`IORING_SETUP_SUBMIT_ALL` 5.18, `IORING_SETUP_DEFER_TASKRUN` 6.1, `IORING_OP_{FSYNC,READ_FIXED,WRITE_FIXED,FALLOCATE,OPENAT,CLOSE,STATX,RENAMEAT,UNLINKAT,MKDIRAT}` all `<= 5.15`, `IOSQE_IO_LINK` 5.3).

## Direct-I/O Write + `fdatasync` Latency Benchmark (`file_uring --bench --iters 200`)

4 KiB `O_DIRECT` overwrite + `IORING_FSYNC_DATASYNC` into a preallocated 256 KiB file (20 warmup iterations, 200 measured iterations), comparing linked chain (`Io::submit_chain` with `IOSQE_IO_LINK`) vs. two separate submissions (`Io::submit` + `tick` for write, then `Io::submit` + `tick` for sync). Raw JSONL in `bench/results/m2-file-bench.jsonl`.

| Host (`arch`, `kernel`, `fs`) | Mode | `p50` (µs) | `p95` (µs) | `p99` (µs) | `mean` (µs) | Chain vs. Two-Op (`p50` / `mean`) |
|---|---|---:|---:|---:|---:|---|
| `donbot` (`aarch64`, `7.0.0-1019-nvidia`, `ext4`) | `two_ops` | 1911.2 | 5261.7 | 6266.0 | 2457.0 | — |
| `donbot` (`aarch64`, `7.0.0-1019-nvidia`, `ext4`) | `chain` | 1600.8 | 2627.0 | 2887.9 | 1639.9 | `-16.2%` `p50` / `-33.3%` `mean` |
| `10.0.0.3` (`x86_64`, `6.8.12-11-pve`, `xfs`) | `two_ops` | 280.6 | 831.7 | 2890.2 | 376.6 | — |
| `10.0.0.3` (`x86_64`, `6.8.12-11-pve`, `xfs`) | `chain` | 265.3 | 2789.0 | 3523.7 | 492.7 | `-5.5%` `p50` / `+30.8%` `mean` |

## S3 / HTTP `io_uring` Before-and-After Comparison (Task 2.6)

Loopback throughput benchmark on `donbot` (`aarch64`, Cortex-X925 pinned `--cpu 19`, 5 runs × 3 s, `8 MiB` objects, `--conns 8`, `--batch 1048576`) against `http_uring sink` (raw JSONL in `bench/results/m1-http-uring-donbot.jsonl` and `bench/results/m2-http-uring-donbot.jsonl`):

### Before vs. After M2 Comparison

| Workload | M1 Baseline Median (min–max) | Post-M2 Median (min–max) | M1 GB/CPU-s | Post-M2 GB/CPU-s | Delta (Median) |
|---|---|---|---|---|---|
| Plain PUT | `5.85 GB/s` (`4.80 to 6.22`) | `5.76 GB/s` (`4.83 to 6.41`) | `5.9` (`4.8 to 6.2`) | `5.8` (`4.8 to 6.4`) | `-1.5%` |
| CRC PUT (`--crc 1`) | `4.42 GB/s` (`4.18 to 4.71`) | `4.65 GB/s` (`4.15 to 5.33`) | `4.4` (`4.2 to 4.7`) | `4.7` (`4.2 to 5.3`) | `+5.2%` |
| Plain GET | `10.91 GB/s` (`10.46 to 11.02`) | `11.04 GB/s` (`10.83 to 11.10`) | `11.4` (`11.0 to 11.5`) | `11.6` (`11.4 to 11.7`) | `+1.2%` |
| TLS PUT | `2.96 GB/s` (`2.48 to 3.48`) | `2.83 GB/s` (`2.20 to 3.15`) | `3.8` (`3.5 to 4.3`) | `3.6` (`3.4 to 4.3`) | `-4.4%` |
| TLS GET | `4.37 GB/s` (`4.34 to 4.42`) | `4.36 GB/s` (`4.29 to 4.36`) | `4.5` (`4.4 to 4.5`) | `4.5` (`4.4 to 4.5`) | `-0.2%` |

### `scripts/bench-summary.py` Output (`bench/results/m2-http-uring-donbot.jsonl`)

| cmd | tls | crc | links | conns | threads | size | batch | arch | runs | GB/s (median, low to high) | thread (median, low to high) | GB per CPU-s (median, low to high) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| put | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 5.76 (4.83 to 6.41) | 1.00 (1.00 to 1.00) | 5.8 (4.8 to 6.4) |
| put | False | True | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.65 (4.15 to 5.33) | 1.00 (1.00 to 1.00) | 4.7 (4.2 to 5.3) |
| get | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 11.04 (10.83 to 11.10) | 0.95 (0.95 to 0.96) | 11.6 (11.4 to 11.7) |
| put | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 2.83 (2.20 to 3.15) | 0.76 (0.65 to 0.82) | 3.6 (3.4 to 4.3) |
| get | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.36 (4.29 to 4.36) | 0.98 (0.97 to 0.98) | 4.5 (4.4 to 4.5) |
