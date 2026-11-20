# Milestone M1 (`#41 add-portable-io-backend`) — Real-Kernel Verification & Compatibility Report

Recorded 2026-11-19 on branch `feat/ring-per-core`.

## Real-Kernel Verification (`scripts/test-linux.sh`)

Executed `unset CARGO_TARGET_DIR && ./scripts/test-linux.sh` against both configured Linux test hosts:

| Target Triple | Host | Kernel (`uname -r`) | Checks Run | Result |
|---|---|---|---|---|
| `aarch64-unknown-linux-musl` | `mfreeman@donbot` | `7.0.0-1019-nvidia` | `contract_uring` (`--v6`, `--posix`, `--posix --v6`), `echo_uring` (7 modes), `http_uring selftest`, `client_uring` (4 modes incl. `--threads 4 --v6`, `--path-style`, `--rotate`), `client_faults` (5 modes), `dns_uring` (IPv4 & `prefer6`) | **Passed** |
| `x86_64-unknown-linux-musl` | `root@10.0.0.3` (`pve02`) | `6.8.12-11-pve` | Identical 23 real-kernel check invocations (`io_uring` + `epoll` POSIX backend) | **Passed** |

- `scripts/test-linux.sh` missing/empty host-file enforcement verified: `LONGSHORE_TEST_HOSTS_FILE=/nonexistent ./scripts/test-linux.sh` exited with status `1` and reported that `io_uring` was not exercised.

## macOS Portable (`kqueue`) & Workspace Verification

Executed on `Darwin 25.6.0 arm64`:
- `python3 scripts/license-headers.py --check`: passed.
- `cargo fmt --all --check`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo clippy -p longshore_common --all-targets --features fault-injection -- -D warnings`: passed.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace --exclude longshore-bench --features longshore/sim`: passed.
- `cargo test --workspace`: passed (including `longshore_common` `posix`, `posix_alloc` under `alloc_guard::sealed`, `posix_contract`, `sim_traces`, and `longshore_s3` `client_portable` loopback + macOS pinning rejection).
- `cargo test -p longshore_common --features fault-injection`: passed (seeded stale-readiness tests alone and among real kernel reports).

## Linux 6.1 Syscall Compatibility Inspection

No Linux 6.1 host is configured in `~/.config/longshore/test-hosts` (hosts run `6.8.12-11-pve` and `7.0.0-1019-nvidia`). Static inspection of `crates/longshore_common/src/io/sys.rs`, `io/uring.rs`, `io/uring/backend.rs`, `io/uring/completions.rs`, and `io/posix/abi_linux.rs` confirms no feature newer than Linux 6.1 is used:
- `io_uring` setup flags: `IORING_SETUP_SINGLE_ISSUER` (6.0) and `IORING_SETUP_DEFER_TASKRUN` (6.1).
- `io_uring` opcodes/flags: `IORING_OP_SOCKET` (5.19), `IORING_OP_SEND_ZC` (6.0), `IORING_RECVSEND_FIXED_BUF` (6.0), `IORING_OP_ACCEPT`, `IORING_OP_CONNECT`, `IORING_OP_SEND`, `IORING_OP_RECV`, `IORING_OP_CLOSE`, `IORING_OP_ASYNC_CANCEL`, `IORING_OP_TIMEOUT`, `IORING_OP_TIMEOUT_REMOVE`, `IORING_OP_OPENAT`, `IORING_OP_READ`. `IORING_SEND_ZC_REPORT_USAGE` (6.2) is defined in `sys.rs` but not submitted in any SQE.
- Portable `epoll` backend (`io/posix/abi_linux.rs` & `epoll.rs`): standard `epoll_create1(EPOLL_CLOEXEC)`, `epoll_ctl`, `epoll_wait`, `eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK)`, `accept4(..., SOCK_NONBLOCK | SOCK_CLOEXEC)`, `socket`, `connect`, `send`, `recv`, `close`, `getsockopt`, `getpeername`, `fcntl(F_DUPFD_CLOEXEC)`, `open`, `pread`, `clock_gettime`.

## Post-Split `io_uring` Benchmark (Task 6.4)

Loopback throughput benchmark on `donbot` (`aarch64`, Cortex-X925 pinned `--cpu 19`, 5 runs × 3 s, `8 MiB` objects, `--conns 8`) against `http_uring sink` (raw JSONL in `bench/results/m1-http-uring-donbot.jsonl`):

### Before vs. After M1 Comparison

| Workload | Baseline Median (min–max) | Post-M1 Median (min–max) | Baseline GB/CPU-s | Post-M1 GB/CPU-s | Delta (Median) |
|---|---|---|---|---|---|
| Plain PUT | `5.40 GB/s` (`4.76 to 6.35`) | `5.85 GB/s` (`4.80 to 6.22`) | `5.4` (`4.8 to 6.4`) | `5.9` (`4.8 to 6.2`) | `+8.3%` |
| CRC PUT (`--crc 1`) | `4.34 GB/s` (`4.21 to 4.97`) | `4.42 GB/s` (`4.18 to 4.71`) | `4.3` (`4.2 to 5.0`) | `4.4` (`4.2 to 4.7`) | `+1.8%` |
| Plain GET | `11.02 GB/s` (`10.83 to 11.24`) | `10.91 GB/s` (`10.46 to 11.02`) | `11.5` (`11.4 to 11.8`) | `11.4` (`11.0 to 11.5`) | `-1.0%` |
| TLS PUT | `3.21 GB/s` (`2.68 to 3.65`) | `2.96 GB/s` (`2.48 to 3.48`) | `4.0` (`3.7 to 4.2`) | `3.8` (`3.5 to 4.3`) | `-7.8%` (within noise; CPU-normalized `3.8` vs `4.0`) |
| TLS GET | `4.35 GB/s` (`4.28 to 4.36`) | `4.37 GB/s` (`4.34 to 4.42`) | `4.5` (`4.4 to 4.5`) | `4.5` (`4.4 to 4.5`) | `+0.5%` |

### `scripts/bench-summary.py` Output (`bench/results/m1-http-uring-donbot.jsonl`)

| cmd | tls | crc | links | conns | threads | size | batch | arch | runs | GB/s (median, low to high) | thread (median, low to high) | GB per CPU-s (median, low to high) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| put | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 5.85 (4.80 to 6.22) | 1.00 (1.00 to 1.00) | 5.9 (4.8 to 6.2) |
| put | False | True | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.42 (4.18 to 4.71) | 1.00 (1.00 to 1.00) | 4.4 (4.2 to 4.7) |
| get | False | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 10.91 (10.46 to 11.02) | 0.95 (0.95 to 0.96) | 11.4 (11.0 to 11.5) |
| put | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 2.96 (2.48 to 3.48) | 0.83 (0.67 to 0.88) | 3.8 (3.5 to 4.3) |
| get | True | False | 1 | 8 | 1 | 8388608 | 1048576 | aarch64 | 5 | 4.37 (4.34 to 4.42) | 0.98 (0.97 to 0.99) | 4.5 (4.4 to 4.5) |
