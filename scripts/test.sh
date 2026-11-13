#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# The gate's test command: unit, simulation and property tests on this
# machine, then the real io_uring checks on Linux hosts.
set -eu
cd "$(dirname "$0")/.."

cargo test --workspace --quiet
exec ./scripts/test-linux.sh
