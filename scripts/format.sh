#
# Copyright (c) 2026. Marvin Hansen. All Rights Reserved.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# bin/sh
set -o errexit
set -o nounset
set -o pipefail

# Rust code formatting
# https://github.com/rust-lang/rustfmt
command cargo fmt --all

