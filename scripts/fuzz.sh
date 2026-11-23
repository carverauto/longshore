#!/usr/bin/env bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Coverage-guided fuzzing of the parsers with cargo-fuzz (libFuzzer).
#
#   scripts/fuzz.sh                       # every target for 60 s each
#   FUZZ_SECONDS=600 scripts/fuzz.sh      # longer runs
#   FUZZ_TARGETS="http_head dns_response" scripts/fuzz.sh
#   FUZZ_SECONDS=0 scripts/fuzz.sh        # build the targets only
#   make fuzz                             # the same, from the Makefile
#
# Settings come from the environment, because the Makefile sources this
# script:
#   FUZZ_SECONDS    run time per target (default 60; 0 builds only)
#   FUZZ_TARGETS    space-separated targets (default: every target in fuzz/)
#   FUZZ_TOOLCHAIN  nightly toolchain for cargo-fuzz (default: nightly)
#
# Needs a nightly toolchain and cargo-fuzz:
#   rustup toolchain install nightly
#   cargo install --locked cargo-fuzz
#
# Each target keeps its corpus in fuzz/corpus/<target> between runs (not
# committed). A crash stops the run, leaves the input under
# fuzz/artifacts/<target>/, and makes the script exit non-zero; replay it
# with `cargo +nightly fuzz run <target> <file>` from fuzz/.

set -o errexit
set -o nounset
set -o pipefail

fuzz_main() {
    local root seconds toolchain targets sanitizer target
    root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
    seconds=${FUZZ_SECONDS:-60}
    toolchain=${FUZZ_TOOLCHAIN:-nightly}

    if ! command -v cargo-fuzz >/dev/null 2>&1; then
        echo "fuzz: cargo-fuzz is not installed: cargo install --locked cargo-fuzz" >&2
        return 1
    fi
    if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
        echo "fuzz: toolchain $toolchain is not installed: rustup toolchain install $toolchain" >&2
        return 1
    fi

    # The nightly AddressSanitizer runtime hangs at startup on macOS. The
    # parsers contain no unsafe code, so bounds errors still surface as
    # panics without it (see fuzz/Cargo.toml).
    sanitizer=()
    if [ "$(uname -s)" = Darwin ]; then
        sanitizer=(-s none)
    fi

    cd "$root/fuzz"
    targets=${FUZZ_TARGETS:-$(cargo "+$toolchain" fuzz list)}

    if [ "$seconds" = 0 ]; then
        echo "== building the fuzz targets"
        cargo "+$toolchain" fuzz build ${sanitizer[@]+"${sanitizer[@]}"}
        return 0
    fi

    for target in $targets; do
        echo "== $target for ${seconds}s"
        mkdir -p "corpus/$target"
        cargo "+$toolchain" fuzz run ${sanitizer[@]+"${sanitizer[@]}"} "$target" "corpus/$target" -- \
            -max_total_time="$seconds" -print_final_stats=1
    done
    echo "== fuzzing passed: $(echo "$targets" | wc -w | tr -d ' ') targets, ${seconds}s each"
}

# Sourced by the Makefile; run in a subshell so `cd` and errexit stay here.
(fuzz_main)
