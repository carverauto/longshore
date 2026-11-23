# Milestone M5a (`#45 add-core-mailbox`) — Loom Model Suite Runtime Comparison

Recorded 2026-11-19 on branch `feat/ring-per-core` (`--cfg longshore_model --release --target-dir target/model`).

## Before vs. After `./scripts/test-model.sh` Runtime

| Phase | Baseline (`baseline-test-model.txt`, pre-M5a) | Post-M5a (`./scripts/test-model.sh`) | Notes |
|---|---|---|---|
| `longshore_mailbox` (`mailbox_model`, 12 tests) | N/A | `1.01s` test (`0.11s` warm / `7.25s` compile) | Queue (2 senders + 1 receiver, full/wraparound, in-band `stop` when full, `send` racing `close`, `wait_stop` racing `stop`, `try_send` racing `stop` under `try_recv`/`close`/`try_recv`+`close`) + Slots (2 waiters on 1 slot, future + blocking caller, `detach` freeing slot before `u32` tag, two-phase `close`) |
| `longshore_s3` (`client_model`, 10 tests) | `298.72s` test (`7m30s` cold compile) | `22.08s` test (`0.09s` warm / `13.00s` incremental compile) | Unedited `tests/client/model.rs` & `model/machine.rs`; identical order counts across all 10 tests (`740`–`158,631` orders) |
| Total `./scripts/test-model.sh` wall time | `12:29.99` (`749.99s`, cold `target/model`) | `23.365s` (`24.44s` user, `24.24s` sys) | Both suites pass (`12/12` + `10/10`) |
