#!/usr/bin/env bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Self-hosted S3 stores in containers, and the store programs run against
# them from a second container on the same Docker network. STORE is `ceph`
# (a one-node Ceph cluster with its S3 gateway, RGW) or `rustfs`. Neither is
# given a DNS name, so both serve only path-style requests: the bucket goes
# in the path (`S3_PATH_STYLE=1`).
#
#   scripts/self-hosted-s3.sh up STORE     # start it, with a user and a bucket
#   scripts/self-hosted-s3.sh run STORE    # build s3_uring and exact_s3, run them against it
#   scripts/self-hosted-s3.sh down         # remove every container and the network
#
# The programs are built in a `rust:1-bookworm` container (target directory
# in the `longshore-target` volume) and run in the same image with the
# seccomp profile off, since Docker's default profile refuses io_uring, and
# with no limit on locked memory. Plain HTTP on port 80, which is the port
# the programs use without TLS. Image tags: CEPH_IMAGE, RUSTFS_IMAGE,
# RUST_IMAGE.
#
# `ceph-inside` is what the Ceph container runs; it is not meant to be
# called by hand.

set -euo pipefail

CEPH_IMAGE=${CEPH_IMAGE:-quay.io/ceph/ceph:v20.2.5}
RUSTFS_IMAGE=${RUSTFS_IMAGE:-rustfs/rustfs:1.0.6}
RUST_IMAGE=${RUST_IMAGE:-rust:1-bookworm}
NET=longshore-s3
ACCESS=longshore-access-key
SECRET=longshore-secret-key-for-local-tests
BUCKET=longshore-test
REGION=us-east-1

ceph_inside() {
    local fsid conf=/etc/ceph/ceph.conf ip
    fsid=$(cat /proc/sys/kernel/random/uuid)
    ip=$(hostname -i | awk '{print $1}')
    mkdir -p /etc/ceph /var/lib/ceph/{mon/ceph-a,mgr/ceph-x,osd/ceph-0,radosgw/ceph-rgw.a} \
        /var/log/ceph /run/ceph
    cat >"$conf" <<EOF
[global]
fsid = $fsid
mon host = $ip
auth allow insecure global id reclaim = false
osd pool default size = 1
osd pool default min size = 1
mon allow pool size one = true
mon warn on pool no redundancy = false
osd crush chooseleaf type = 0
mon data avail warn = 2
mon data avail crit = 1
log to stderr = false
log file = /var/log/ceph/\$name.log
osd objectstore = bluestore
bluestore block create = true
bluestore block size = 8589934592
osd memory target = 1073741824
[client.rgw.a]
rgw frontends = beast port=80
EOF
    # cephx with generated keys: the monitor's, the admin's (for the `ceph`
    # and `radosgw-admin` commands here), then one per daemon.
    local keys=/etc/ceph/ceph.mon.keyring admin=/etc/ceph/ceph.client.admin.keyring
    ceph-authtool --create-keyring "$keys" --gen-key -n mon. --cap mon 'allow *' >/dev/null
    ceph-authtool --create-keyring "$admin" --gen-key -n client.admin \
        --cap mon 'allow *' --cap osd 'allow *' --cap mgr 'allow *' --cap mds 'allow *' >/dev/null
    ceph-authtool "$keys" --import-keyring "$admin" >/dev/null
    monmaptool --create --addv a "[v2:$ip:3300,v1:$ip:6789]" --fsid "$fsid" /tmp/monmap >/dev/null
    ceph-mon --mkfs -i a --monmap /tmp/monmap --keyring "$keys"
    ceph-mon -i a
    ceph auth get-or-create mgr.x mon 'allow profile mgr' osd 'allow *' mds 'allow *' \
        -o /var/lib/ceph/mgr/ceph-x/keyring
    ceph-mgr -i x
    local uuid secret
    uuid=$(cat /proc/sys/kernel/random/uuid)
    secret=$(ceph-authtool --gen-print-key)
    echo "{\"cephx_secret\": \"$secret\"}" | ceph osd new "$uuid" 0 -i - >/dev/null
    ceph-authtool --create-keyring /var/lib/ceph/osd/ceph-0/keyring --name osd.0 \
        --add-key "$secret" >/dev/null
    ceph-osd -i 0 --mkfs --osd-uuid "$uuid" 2>/dev/null
    ceph osd crush add osd.0 1 root=default >/dev/null
    ceph-osd -i 0 2>/dev/null
    ceph auth get-or-create client.rgw.a mon 'allow rw' osd 'allow rwx' mgr 'allow rw' \
        -o /var/lib/ceph/radosgw/ceph-rgw.a/keyring
    radosgw -n client.rgw.a
    for _ in $(seq 120); do
        if (exec 3<>/dev/tcp/127.0.0.1/80) 2>/dev/null; then
            radosgw-admin user create --uid=longshore --display-name=longshore \
                --access-key="$ACCESS" --secret="$SECRET" >/dev/null
            echo "ceph-rgw: ready, $(ceph --version)"
            exec tail -f /dev/null
        fi
        sleep 1
    done
    echo "ceph-rgw: the gateway did not come up" >&2
    tail -n 50 /var/log/ceph/*.log >&2
    exit 1
}

node() {
    case "$1" in
    ceph | rustfs) echo "longshore-$1" ;;
    *)
        echo "STORE is ceph or rustfs" >&2
        exit 2
        ;;
    esac
}

up() {
    local name
    name=$(node "$1")
    docker network inspect "$NET" >/dev/null 2>&1 || docker network create "$NET" >/dev/null
    docker rm -f "$name" >/dev/null 2>&1 || true
    if [ "$1" = ceph ]; then
        docker run -d --name "$name" --hostname "$name" --network "$NET" \
            -v "$PWD/scripts/self-hosted-s3.sh":/self-hosted-s3.sh:ro --entrypoint bash \
            "$CEPH_IMAGE" /self-hosted-s3.sh ceph-inside >/dev/null
        wait_for "$name" "ceph-rgw: ready"
    else
        docker run -d --name "$name" --hostname "$name" --network "$NET" \
            -e RUSTFS_ADDRESS=:80 -e RUSTFS_REGION="$REGION" \
            -e RUSTFS_ACCESS_KEY="$ACCESS" -e RUSTFS_SECRET_KEY="$SECRET" \
            "$RUSTFS_IMAGE" >/dev/null
        wait_for "$name" ""
    fi
    make_bucket "$name"
}

# Wait for a line in the container's log, or (with no line given) for
# its port 80 to accept a connection.
wait_for() {
    for _ in $(seq 180); do
        if [ "$(docker inspect -f '{{.State.Running}}' "$1")" != true ]; then
            docker logs "$1" >&2
            exit 1
        fi
        if [ -n "$2" ]; then
            docker logs "$1" 2>&1 | grep "$2" && return
        elif docker run --rm --network "$NET" --entrypoint bash "$RUST_IMAGE" \
            -c "exec 3<>/dev/tcp/$1/80" 2>/dev/null; then
            echo "$1: listening"
            return
        fi
        sleep 1
    done
    echo "timed out waiting for $1" >&2
    exit 1
}

# The store programs do not create buckets, so the bucket is made with one
# signed request from Python, in a throwaway container on the network.
make_bucket() {
    docker run --rm --network "$NET" --entrypoint python3 \
        -e HOST="$1" -e ACCESS="$ACCESS" -e SECRET="$SECRET" -e BUCKET="$BUCKET" -e REGION="$REGION" \
        "$CEPH_IMAGE" -c '
import datetime, hashlib, hmac, os, urllib.request, urllib.error
e = os.environ
a, s, b, host, region = e["ACCESS"], e["SECRET"], e["BUCKET"], e["HOST"], e["REGION"]
now = datetime.datetime.now(datetime.timezone.utc)
amz, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
empty = hashlib.sha256(b"").hexdigest()
signed = "host;x-amz-content-sha256;x-amz-date"
canon = f"PUT\n/{b}\n\nhost:{host}\nx-amz-content-sha256:{empty}\nx-amz-date:{amz}\n\n{signed}\n{empty}"
scope = f"{day}/{region}/s3/aws4_request"
sts = f"AWS4-HMAC-SHA256\n{amz}\n{scope}\n" + hashlib.sha256(canon.encode()).hexdigest()
k = ("AWS4" + s).encode()
for part in (day, region, "s3", "aws4_request"):
    k = hmac.new(k, part.encode(), hashlib.sha256).digest()
sig = hmac.new(k, sts.encode(), hashlib.sha256).hexdigest()
req = urllib.request.Request(f"http://{host}/{b}", method="PUT", headers={
    "x-amz-date": amz, "x-amz-content-sha256": empty,
    "Authorization": f"AWS4-HMAC-SHA256 Credential={a}/{scope}, SignedHeaders={signed}, Signature={sig}"})
try:
    urllib.request.urlopen(req)
    print(f"{host}: bucket {b} created")
except urllib.error.HTTPError as err:
    body = err.read().decode()
    if "BucketAlreadyOwnedByYou" in body or "BucketAlreadyExists" in body:
        print(f"{host}: bucket {b} exists")
    else:
        raise SystemExit(f"{host}: creating the bucket: {err.code} {body}")
'
}

# Run a command in the Rust image on the network, with the repository and
# the build volumes mounted and the store named in the environment.
rusty() {
    local name=$1
    shift
    docker run --rm --network "$NET" \
        --security-opt seccomp=unconfined --ulimit memlock=-1:-1 \
        -v "$PWD":/src:ro -v longshore-cargo:/usr/local/cargo/registry \
        -v longshore-target:/target -e CARGO_TARGET_DIR=/target -w /src \
        -e S3_ENDPOINT="$name" -e S3_REGION="$REGION" -e S3_BUCKET="$BUCKET" \
        -e S3_ACCESS_KEY="$ACCESS" -e S3_SECRET_KEY="$SECRET" -e S3_PATH_STYLE=1 \
        "$RUST_IMAGE" "$@"
}

run() {
    local name
    name=$(node "$1")
    rusty "$name" cargo build --release --quiet -p longshore-bench --bin s3_uring --bin exact_s3
    echo "== commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates || echo ' (with local changes)')"
    echo "== kernel: $(rusty "$name" uname -srm)"
    if [ "$1" = ceph ]; then
        echo "== store: $(docker exec "$name" ceph --version)"
    else
        echo "== store: $(docker exec "$name" rustfs --version | head -1)"
    fi
    local prog
    for prog in "s3_uring selftest" "s3_uring objects" "s3_uring multipart" "s3_uring badcrc" "exact_s3"; do
        echo "== $prog"
        # shellcheck disable=SC2086
        rusty "$name" /target/release/$prog || echo "== $prog: exit status $?"
    done
}

down() {
    docker rm -f longshore-ceph longshore-rustfs >/dev/null 2>&1 || true
    docker network rm "$NET" >/dev/null 2>&1 || true
}

case "${1:-}" in
ceph-inside) ceph_inside ;;
up) up "${2:-}" ;;
run) run "${2:-}" ;;
down) down ;;
*)
    echo "usage: $0 up STORE | run STORE | down   (STORE is ceph or rustfs)" >&2
    exit 2
    ;;
esac
