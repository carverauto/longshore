#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Builds for AArch64 with AWS-LC told at compile time that the CPU has the
# AES, PMULL and SHA3 instructions and should use its 8-way AES-GCM routine.
#
# AWS-LC picks that routine by itself only on CPUs it recognises (Neoverse
# V1, V2, V3 and Apple M). Other cores with the same instructions, such as
# the Cortex-X925 in the DGX Spark, get a slower routine: 7.8 against
# 12.2 GiB/s for sealing, and 5.9 against 7.5 GB/s for an HTTPS upload
# (bench/results/m11-tls-fast-aes-aarch64-dgx-spark.md).
#
# A binary built this way will crash with an illegal instruction on a CPU
# that lacks any of those instructions. Use it only where the hardware is
# known. It is not needed on x86-64, on Graviton, or on Apple silicon.
#
# Usage: scripts/build-fast-aes-aarch64.sh [cargo build arguments]
#   e.g. scripts/build-fast-aes-aarch64.sh --release -p longshore-bench --bin http_uring
# Output goes to target/fast-aes/, so it never mixes with a normal build.
set -eu
cd "$(dirname "$0")/.."
TARGET="${LONGSHORE_FAST_AES_TARGET:-aarch64-unknown-linux-musl}"
VAR="CFLAGS_$(echo "$TARGET" | tr '-' '_')"
FLAGS="-DOPENSSL_STATIC_ARMCAP -DOPENSSL_STATIC_ARMCAP_NEON -DOPENSSL_STATIC_ARMCAP_AES \
-DOPENSSL_STATIC_ARMCAP_PMULL -DOPENSSL_STATIC_ARMCAP_SHA1 -DOPENSSL_STATIC_ARMCAP_SHA256 \
-DOPENSSL_STATIC_ARMCAP_SHA512 -DOPENSSL_STATIC_ARMCAP_SHA3 -DOPENSSL_STATIC_ARMCAP_NEOVERSE_V3"
export "$VAR=$FLAGS"
exec env CARGO_TARGET_DIR=target/fast-aes cargo build --target "$TARGET" "$@"
