#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Runs the model-checked tests of the shareable client's hand-offs between
# callers and I/O threads (crates/longshore_s3/tests/client/model.rs).
#
# They need the crate built with `--cfg longshore_model`, which swaps its
# locks, atomics and thread parking for the model checker's. That is a
# different build of everything, so it has a target directory of its own.
# Runs anywhere the simulator does, macOS included. About a minute once built.
set -eu
cd "$(dirname "$0")/.."

RUSTFLAGS="${RUSTFLAGS:-} --cfg longshore_model" \
    cargo test -p longshore_mailbox --release --target-dir target/model \
    --test mailbox_model -- --nocapture

RUSTFLAGS="${RUSTFLAGS:-} --cfg longshore_model" \
    cargo test -p longshore_s3 --release --target-dir target/model \
    --test client_model -- --nocapture
