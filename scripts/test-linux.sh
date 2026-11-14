#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Runs the real-kernel checks on Linux hosts over SSH.
#
# io_uring code is only compiled for Linux, so `cargo test` on a Mac never
# executes it. This script cross-compiles the check binaries, copies them to
# each configured host and runs them there.
#
# Hosts come from $LONGSHORE_TEST_HOSTS_FILE (default ~/.config/longshore/test-hosts),
# one "<rust-target> <ssh-destination>" per line, for example:
#
#   aarch64-unknown-linux-musl user@arm-host
#   x86_64-unknown-linux-musl  user@x86-host
#
# Without that file the Linux checks are skipped with a warning: the gate is
# then weaker, and says so.
set -eu
cd "$(dirname "$0")/.."

HOSTS="${LONGSHORE_TEST_HOSTS_FILE:-$HOME/.config/longshore/test-hosts}"
if [ ! -f "$HOSTS" ]; then
    echo "WARNING: $HOSTS not found; SKIPPING real io_uring checks." >&2
    echo "WARNING: nothing in this run exercised the Linux I/O path." >&2
    exit 0
fi

ID="gate-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)-$$"
# -n: ssh must not read the host list that the loop below is reading.
SSH="ssh -n -o BatchMode=yes -o ConnectTimeout=10"
expected=$(grep -c -v '^[[:space:]]*\(#\|$\)' "$HOSTS" || true)
ran=0
failed=0

# Read the host list on descriptor 3 so nothing in the loop can consume it.
while read -r target dest <&3; do
    case "$target" in ''|\#*) continue ;; esac
    echo "== $target on $dest"
    cargo build --release --quiet --target "$target" -p longshore-bench --bin echo_uring --bin http_uring --bin dns_uring --bin client_uring
    # Built apart from the rest: its feature lets a test break a ring, and
    # no other program is built with that in it.
    cargo build --release --quiet --target "$target" -p longshore-bench --features fault-injection --bin client_faults
    bin="target/$target/release"
    $SSH "$dest" "mkdir -p longshore-bench/$ID"
    scp -q -o BatchMode=yes "$bin/echo_uring" "$bin/http_uring" "$bin/dns_uring" "$bin/client_uring" "$bin/client_faults" "$dest:longshore-bench/$ID/" </dev/null
    # Each check seals the heap after setup and exits non-zero on any failure.
    if $SSH "$dest" "cd longshore-bench/$ID && set -e
        timeout 60 ./echo_uring
        timeout 60 ./echo_uring --zc
        timeout 60 ./echo_uring --total 1000003 --chunk 4096 --zc
        timeout 60 ./echo_uring --v6
        timeout 60 ./echo_uring --v6 --zc
        timeout 60 ./echo_uring --measure --zc
        timeout 60 ./echo_uring --v6 --measure
        timeout 60 ./http_uring selftest
        timeout 60 ./client_uring
        timeout 60 ./client_uring --threads 4 --v6
        timeout 60 ./client_uring --path-style
        timeout 90 ./client_uring --rotate
        timeout 250 ./client_faults break tick
        timeout 250 ./client_faults break ring
        timeout 250 ./client_faults break tick --threads 4
        timeout 250 ./client_faults break ring --threads 4
        timeout 250 ./client_faults churn
        timeout 30 ./dns_uring example.com
        DNS_FAMILY=prefer6 timeout 30 ./dns_uring example.com"; then
        echo "== $target on $dest: ok"
    else
        echo "== $target on $dest: FAILED" >&2
        failed=$((failed + 1))
    fi
    ran=$((ran + 1))
    $SSH "$dest" "rm -rf longshore-bench/$ID" || true
done 3<"$HOSTS"

if [ "$failed" -ne 0 ] || [ "$ran" -ne "$expected" ] || [ "$ran" -eq 0 ]; then
    echo "real io_uring checks: $ran of $expected hosts ran, $failed failed" >&2
    exit 1
fi
echo "real io_uring checks passed on $ran of $expected configured hosts"
