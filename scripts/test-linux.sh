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
# Without that file (or with zero hosts listed) the script fails so the
# Linux io_uring path is never silently skipped.
set -eu
cd "$(dirname "$0")/.."

HOSTS="${LONGSHORE_TEST_HOSTS_FILE:-$HOME/.config/longshore/test-hosts}"
if [ ! -f "$HOSTS" ]; then
    echo "ERROR: $HOSTS not found; io_uring was not exercised." >&2
    echo "ERROR: see README.md for setting LONGSHORE_TEST_HOSTS_FILE or ~/.config/longshore/test-hosts." >&2
    exit 1
fi

ID="gate-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)-$$"
# -n: ssh must not read the host list that the loop below is reading.
SSH="ssh -n -o BatchMode=yes -o ConnectTimeout=10"
SSH_RAW="ssh -o BatchMode=yes -o ConnectTimeout=10"
SKIP_E2E="--skip t1_f01_5 --skip t1_f18_5 --skip t1_f19_4 --skip t1_f19_5 --skip t2_f19_3 --skip t2_f19_4 --skip t1_f26_4 --skip t1_f27_5 --skip t4_04_cargo_tree_no_default_features_excludes_tls_and_aws_lc"
expected=$(grep -c -v '^[[:space:]]*\(#\|$\)' "$HOSTS" || true)
if [ "$expected" -eq 0 ]; then
    echo "ERROR: $HOSTS lists 0 hosts; io_uring was not exercised." >&2
    exit 1
fi
ran=0
failed=0

e2e_bin() {
    cargo test --quiet --no-run --message-format=json "$@" | python3 -c '
import json, sys
for line in sys.stdin:
    if not line.startswith("{"):
        continue
    d = json.loads(line)
    if d.get("reason") == "compiler-artifact" and d.get("target", {}).get("name") == "e2e" and d.get("executable"):
        print(d["executable"])
' | tail -n 1
}

# Read the host list on descriptor 3 so nothing in the loop can consume it.
while read -r target dest <&3; do
    case "$target" in ''|\#*) continue ;; esac
    echo "== $target on $dest"
    cargo build --release --quiet --target "$target" -p longshore-bench --bin contract_uring --bin server_uring --bin tcp_uring --bin file_uring --bin echo_uring --bin http_uring --bin dns_uring --bin client_uring
    # Built apart from the rest: its feature lets a test break a ring, and
    # no other program is built with that in it.
    cargo build --release --quiet --target "$target" -p longshore-bench --features fault-injection --bin client_faults --bin uring_faults
    e2e_fi=$(e2e_bin --target "$target" -p longshore --features "sim,fault-injection" --test e2e)
    e2e_notls=$(e2e_bin --target "$target" -p longshore --no-default-features --features sim --test e2e)
    [ -n "$e2e_fi" ] && [ -x "$e2e_fi" ]
    [ -n "$e2e_notls" ] && [ -x "$e2e_notls" ]
    bin="${CARGO_TARGET_DIR:-target}/$target/release"
    $SSH "$dest" "mkdir -p longshore-bench/$ID/repo"
    scp -q -o BatchMode=yes "$bin/contract_uring" "$bin/server_uring" "$bin/tcp_uring" "$bin/file_uring" "$bin/echo_uring" "$bin/http_uring" "$bin/dns_uring" "$bin/client_uring" "$bin/client_faults" "$bin/uring_faults" "$dest:longshore-bench/$ID/" </dev/null
    scp -q -o BatchMode=yes "$e2e_fi" "$dest:longshore-bench/$ID/e2e_sim_fi" </dev/null
    scp -q -o BatchMode=yes "$e2e_notls" "$dest:longshore-bench/$ID/e2e_notls_sim" </dev/null
    COPYFILE_DISABLE=1 tar -cf - --exclude=target --exclude=.git . | $SSH_RAW "$dest" "tar -xf - -C longshore-bench/$ID/repo"
    # Each check seals the heap after setup and exits non-zero on any failure.
    if $SSH "$dest" "cd longshore-bench/$ID && set -e
        file_dir=.
        test_dir=\"/tmp/ls-\$\$\"
        fs_type=\$(df -T . | awk 'NR==2 {print \$2}')
        if [ -d /mnt/storage-pve02 ] && [ -w /mnt/storage-pve02 ]; then
            test_dir=\"/mnt/storage-pve02/ls-\$\$\"
            if [ \"\$fs_type\" = zfs ] || [ \"\$fs_type\" = tmpfs ]; then
                file_dir=\"\$test_dir\"
            fi
        fi
        mkdir -p \"\$test_dir\"
        trap 'rm -rf \"\$test_dir\"' EXIT
        export LONGSHORE_REPO_ROOT=\"\$PWD/repo\"
        export LONGSHORE_FILE_URING_BIN=\"\$PWD/file_uring\"
        export LONGSHORE_TEST_DIR=\"\$test_dir\"
        export TMPDIR=\"\$test_dir\"
        timeout 60 ./contract_uring
        timeout 60 ./contract_uring --v6
        timeout 60 ./contract_uring --posix
        timeout 60 ./contract_uring --posix --v6
        timeout 60 ./server_uring
        timeout 60 ./server_uring --v6
        timeout 60 ./server_uring --posix
        timeout 60 ./server_uring --posix --v6
        timeout 60 ./tcp_uring
        timeout 60 ./tcp_uring --v6
        timeout 60 ./file_uring --dir \"\$file_dir\"
        timeout 60 ./file_uring --dir \"\$file_dir\" --posix
        timeout 60 ./file_uring --dir \"\$file_dir\" --bench --interleave --iters 200
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
        timeout 60 ./uring_faults
        timeout 30 ./dns_uring example.com
        DNS_FAMILY=prefer6 timeout 30 ./dns_uring example.com
        ulimit -n 4096
        echo '-- e2e (sim,fault-injection) --'
        timeout -k 5 300 ./e2e_sim_fi $SKIP_E2E
        echo '-- e2e (--no-default-features --features sim) --'
        timeout -k 5 300 ./e2e_notls_sim $SKIP_E2E"; then
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
