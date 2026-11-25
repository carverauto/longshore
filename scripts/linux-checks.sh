#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Runs the real-kernel check programs on the Linux machine it is started on.
# `scripts/test-linux.sh` builds the programs and calls this: here, in a
# container, or on another machine over SSH.
#
#   linux-checks.sh <directory holding the programs> <repository root>
#
# Each check seals the heap after setup and exits non-zero on any failure.
set -eu
bin=$(cd "$1" && pwd)
repo=$(cd "$2" && pwd)
SKIP_E2E="--skip t1_f01_5 --skip t1_f18_5 --skip t1_f19_4 --skip t1_f19_5 --skip t2_f19_3 --skip t2_f19_4 --skip t1_f26_4 --skip t1_f27_5 --skip t4_04_cargo_tree_no_default_features_excludes_tls_and_aws_lc"

# Files for direct I/O go next to the programs: that is a real file system
# wherever they were put, which /tmp (often tmpfs) need not be.
test_dir="/tmp/ls-$$"
file_dir="$bin/ls-files-$$"
if [ -d /mnt/storage-pve02 ] && [ -w /mnt/storage-pve02 ]; then
    test_dir="/mnt/storage-pve02/ls-$$"
    fs_type=$(df -T "$bin" | awk 'NR==2 {print $2}')
    if [ "$fs_type" = zfs ] || [ "$fs_type" = tmpfs ]; then
        file_dir="$test_dir"
    fi
fi
mkdir -p "$test_dir" "$file_dir"
trap 'rm -rf "$test_dir" "$file_dir"' EXIT
export LONGSHORE_REPO_ROOT="$repo"
export LONGSHORE_FILE_URING_BIN="$bin/file_uring"
export LONGSHORE_TEST_DIR="$test_dir"
export TMPDIR="$test_dir"
cd "$bin"

timeout 60 ./contract_uring
timeout 60 ./contract_uring --v6
timeout 60 ./contract_uring --posix
timeout 60 ./contract_uring --posix --v6
timeout 60 ./server_uring
timeout 60 ./server_uring --v6
timeout 60 ./server_uring --posix
timeout 60 ./server_uring --posix --v6
timeout 120 ./tcp_uring --dir "$file_dir"
timeout 120 ./tcp_uring --v6 --dir "$file_dir"
timeout 60 ./file_uring --dir "$file_dir"
timeout 60 ./file_uring --dir "$file_dir" --posix
timeout 60 ./file_uring --dir "$file_dir" --bench --interleave --iters 200
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
# shellcheck disable=SC2086
timeout -k 5 300 ./e2e_sim_fi $SKIP_E2E
echo '-- e2e (--no-default-features --features sim) --'
# shellcheck disable=SC2086
timeout -k 5 300 ./e2e_notls_sim $SKIP_E2E
