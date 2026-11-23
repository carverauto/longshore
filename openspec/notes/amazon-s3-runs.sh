#!/usr/bin/env bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# The runs against Amazon S3 that close `add-reported-operations` 4.5 and
# `add-ipv6` 3.3. See amazon-s3-runs.md next to this file.
#
# From the repository root, on Linux 6.1 or later:
#
#   export S3_BUCKET=... S3_REGION=us-east-1
#   eval "$(aws configure export-credentials --format env)"   # or set S3_ACCESS_KEY etc.
#   openspec/notes/amazon-s3-runs.sh
#
# Everything printed goes to a directory named in the last line. Nothing
# is left in the bucket: every program erases what it wrote.

set -euo pipefail

: "${S3_BUCKET:?set S3_BUCKET to an existing bucket}"
: "${S3_REGION:?set S3_REGION to the region of the bucket}"
export S3_ACCESS_KEY=${S3_ACCESS_KEY:-${AWS_ACCESS_KEY_ID:-}}
export S3_SECRET_KEY=${S3_SECRET_KEY:-${AWS_SECRET_ACCESS_KEY:-}}
export S3_SESSION_TOKEN=${S3_SESSION_TOKEN:-${AWS_SESSION_TOKEN:-}}
if [ -z "$S3_ACCESS_KEY" ] || [ -z "$S3_SECRET_KEY" ]; then
    echo "no keys: set S3_ACCESS_KEY and S3_SECRET_KEY, or AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY" >&2
    exit 2
fi
case "$S3_BUCKET" in *.*)
    echo "a bucket name with dots does not match S3's certificate; use one without" >&2
    exit 2
    ;;
esac
unset S3_PATH_STYLE S3_VENDOR S3_PREFER_LARGE_SEGMENTS

OUT=${OUT:-amazon-s3-runs-$(date -u +%Y%m%dT%H%M%SZ)}
mkdir -p "$OUT"
cargo build --release --quiet -p longshore-bench --bin s3_uring --bin exact_s3
BIN=${CARGO_TARGET_DIR:-target}/release

{
    echo "date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates || echo ' (with local changes)')"
    echo "kernel: $(uname -srm)"
    echo "host: $(hostname)"
    echo "region: $S3_REGION"
    echo "temporary keys: $([ -n "$S3_SESSION_TOKEN" ] && echo yes || echo no)"
    echo "global IPv6 addresses:"
    ip -6 addr show scope global 2>/dev/null | awk '/inet6/ {print "  " $2}'
} | tee "$OUT/host.txt"

# Run one program, keep what it printed, and go on if it fails.
step() {
    local name=$1
    shift
    echo "== $name"
    if "$@" >"$OUT/$name.txt" 2>&1; then
        echo "   ok"
    else
        echo "   exit status $? (see $OUT/$name.txt)"
    fi
}

# 1. add-reported-operations 4.5: put, head and get, over HTTPS, IPv4.
export S3_ENDPOINT=s3.$S3_REGION.amazonaws.com S3_TLS=1 S3_FAMILY=4
step exact-https "$BIN/exact_s3"

# 2. Whether S3 refuses a wrong CRC-64/NVME, and with which error code. The
#    client assumes `BadDigest`; RustFS uses it (bench/results/m27).
#    `s3_uring badcrc` prints only how the client classed the answer, so one
#    signed PUT with a wrong checksum is also sent with curl, and its status
#    line and body are kept: the error code is in that body.
step badcrc-https "$BIN/s3_uring" badcrc

badcrc_curl() {
    local url body="longshore wrong checksum" hash token=()
    url=https://$S3_BUCKET.s3.$S3_REGION.amazonaws.com/longshore-badcrc/$(date -u +%Y%m%dT%H%M%SZ)-$$
    hash=$(printf %s "$body" | sha256sum | cut -d' ' -f1)
    if [ -n "$S3_SESSION_TOKEN" ]; then
        token=(-H "x-amz-security-token: $S3_SESSION_TOKEN")
    fi
    local sign=(--aws-sigv4 "aws:amz:$S3_REGION:s3" --user "$S3_ACCESS_KEY:$S3_SECRET_KEY" ${token[@]+"${token[@]}"})
    curl -sS -i -X PUT "${sign[@]}" \
        -H "Content-Type: application/octet-stream" \
        -H "x-amz-content-sha256: $hash" \
        -H "x-amz-checksum-crc64nvme: AAAAAAAAAAA=" \
        --data-binary "$body" "$url"
    curl -sS -o /dev/null -X DELETE "${sign[@]}" "$url"
}
step badcrc-curl badcrc_curl

# 3. add-ipv6 3.3: the dual-stack endpoint, over IPv6 only, IPv6 first, and
#    IPv4 for comparison, each over HTTP and HTTPS.
if ip -6 addr show scope global 2>/dev/null | grep -q inet6; then
    export S3_ENDPOINT=s3.dualstack.$S3_REGION.amazonaws.com
    for family in 6 prefer6 4; do
        for tls in 0 1; do
            export S3_FAMILY=$family S3_TLS=$tls
            step "dualstack-family$family-tls$tls" "$BIN/s3_uring" selftest
        done
    done
else
    echo "== dual-stack: skipped, this host has no global IPv6 address" | tee "$OUT/dualstack-skipped.txt"
fi

echo "results in $OUT"
