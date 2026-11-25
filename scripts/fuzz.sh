#!/usr/bin/env bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Coverage-guided fuzzing with cargo-fuzz (libFuzzer): the parsers, and
# the TCP connection set's framing and model.
#
#   scripts/fuzz.sh                             # every target for 60 s each
#   scripts/fuzz.sh --seconds 600               # longer runs
#   scripts/fuzz.sh http_head dns_response      # these targets only
#   scripts/fuzz.sh --seconds 0                 # build the targets only
#   make fuzz                                   # the first of these
#
# Everything is a parameter; the script reads nothing from the
# environment:
#   --seconds N        run time per target (default 60; 0 builds only)
#   --toolchain NAME   nightly toolchain for cargo-fuzz (default: nightly)
#   TARGET ...         the targets to run (default: every target in fuzz/)
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
    seconds=60
    toolchain=nightly
    targets=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --seconds | --toolchain)
                if [ $# -lt 2 ]; then
                    echo "fuzz: $1 needs a value" >&2
                    return 2
                fi
                if [ "$1" = --seconds ]; then seconds=$2; else toolchain=$2; fi
                shift 2
                ;;
            -*)
                echo "fuzz: unknown option $1" >&2
                return 2
                ;;
            *)
                targets="$targets $1"
                shift
                ;;
        esac
    done
    case "$seconds" in
        '' | *[!0-9]*)
            echo "fuzz: --seconds takes a whole number, not '$seconds'" >&2
            return 2
            ;;
    esac

    if ! command -v cargo-fuzz >/dev/null 2>&1; then
        echo "fuzz: cargo-fuzz is not installed: cargo install --locked cargo-fuzz" >&2
        return 1
    fi
    if ! rustup run "$toolchain" rustc --version >/dev/null 2>&1; then
        echo "fuzz: toolchain $toolchain is not installed: rustup toolchain install $toolchain" >&2
        return 1
    fi

    # The nightly AddressSanitizer runtime hangs at startup on macOS. The
    # parsers and longshore_tcp contain no unsafe code, so bounds errors
    # still surface as panics without it; what does (the simulator's
    # crate) is fuzzed with it on Linux (see fuzz/Cargo.toml).
    sanitizer=()
    if [ "$(uname -s)" = Darwin ]; then
        sanitizer=(-s none)
    fi

    cd "$root/fuzz"
    if [ -z "$targets" ]; then
        targets=$(cargo "+$toolchain" fuzz list)
    fi

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

fuzz_main "$@"
