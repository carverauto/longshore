#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Runs the real-kernel checks. Needs no configuration.
#
#   scripts/test-linux.sh [<rust-target> <ssh-destination>]...
#
# io_uring code is only compiled for Linux, so `cargo test` on a Mac never
# executes it. This script builds the check programs and runs them
# (`scripts/linux-checks.sh`) on a Linux kernel:
#
# - On Linux: on this machine.
# - Elsewhere: in a Linux container, with `docker` or `podman`, on the
#   kernel of the container host. The container runs this same script.
#
# Then it runs them on every other machine it is told of, over SSH, for a
# kernel or hardware this machine does not have: the machines listed in
# `scripts/test-hosts`, which is part of the repository, and those given as
# parameters. A machine is a Rust target and an SSH destination.
#
# If a kernel cannot be reached, here or on one of those machines, the
# script fails: the io_uring path is never silently skipped.
set -eu
cd "$(dirname "$0")/.."

IMAGE="rust:1-bookworm"
PROGRAMS="contract_uring server_uring tcp_uring file_uring files_uring echo_uring http_uring dns_uring client_uring"
FAULT_PROGRAMS="client_faults uring_faults"

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

# Where cargo puts what it builds, as cargo itself tells it.
target_dir() {
    cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
print(json.load(sys.stdin)["target_directory"])
'
}

# Build every check program, for the Rust target $1 (this machine's when
# empty), and gather them in the directory $2 of cargo's target directory.
# Every step is checked by hand: the caller may be where `set -e` is off.
stage() {
    target_flag=""
    built=$(target_dir) || return 1
    [ -n "$built" ] || return 1
    out="$built/release"
    if [ -n "$1" ]; then
        target_flag="--target $1"
        out="$built/$1/release"
    fi
    staged="$built/$2"
    bins=""
    for p in $PROGRAMS; do bins="$bins --bin $p"; done
    # $target_flag and $bins are lists of words.
    # shellcheck disable=SC2086
    cargo build --release --quiet $target_flag -p longshore-bench $bins || return 1
    # Built apart from the rest: its feature lets a test break a ring, and
    # no other program is built with that in it.
    bins=""
    for p in $FAULT_PROGRAMS; do bins="$bins --bin $p"; done
    # shellcheck disable=SC2086
    cargo build --release --quiet $target_flag -p longshore-bench --features fault-injection $bins || return 1
    # shellcheck disable=SC2086
    e2e_fi=$(e2e_bin $target_flag -p longshore --features "sim,fault-injection" --test e2e) || return 1
    # shellcheck disable=SC2086
    e2e_notls=$(e2e_bin $target_flag -p longshore --no-default-features --features sim --test e2e) || return 1
    [ -n "$e2e_fi" ] || return 1
    [ -x "$e2e_fi" ] || return 1
    [ -n "$e2e_notls" ] || return 1
    [ -x "$e2e_notls" ] || return 1
    rm -rf "$staged" || return 1
    mkdir -p "$staged" || return 1
    for p in $PROGRAMS $FAULT_PROGRAMS; do cp "$out/$p" "$staged/" || return 1; done
    cp "$e2e_fi" "$staged/e2e_sim_fi" || return 1
    cp "$e2e_notls" "$staged/e2e_notls_sim" || return 1
    cp scripts/linux-checks.sh "$staged/" || return 1
}

# The checks on this machine, which runs Linux.
run_here() {
    echo "== $(uname -m), Linux $(uname -r), on this machine"
    stage "" linux-checks
    sh "$staged/linux-checks.sh" "$staged" "$PWD"
    echo "== on this machine: ok"
}

# The checks in a Linux container: this script again, where it is `run_here`.
run_in_container() {
    runtime=""
    for candidate in docker podman; do
        if command -v "$candidate" >/dev/null 2>&1; then
            runtime="$candidate"
            break
        fi
    done
    if [ -z "$runtime" ]; then
        echo "ERROR: this is not Linux and no container runtime (docker, podman) was found; io_uring was not exercised." >&2
        echo "ERROR: install Docker, OrbStack or Podman." >&2
        exit 1
    fi
    echo "== in a container ($runtime, $IMAGE)"
    # io_uring is behind the default seccomp profile, and rings lock memory.
    "$runtime" run --rm \
        --security-opt seccomp=unconfined \
        --ulimit memlock=-1:-1 \
        -v "$PWD":/src:ro \
        -v longshore-cargo:/usr/local/cargo/registry \
        -v longshore-target:/target \
        -e CARGO_TARGET_DIR=/target \
        -w /src \
        "$IMAGE" sh scripts/test-linux.sh --here
}

# The checks on the machine $2, built for the Rust target $1, over SSH.
# A step that fails fails the machine: nothing is run from an earlier build.
run_on_host() {
    id="gate-$(git rev-parse --short HEAD 2>/dev/null || echo unknown)-$$"
    # -n: ssh must not read the host list that the caller's loop is reading.
    ssh_n="ssh -n -o BatchMode=yes -o ConnectTimeout=10"
    ssh_raw="ssh -o BatchMode=yes -o ConnectTimeout=10"
    echo "== $1 on $2"
    stage "$1" "$1/linux-checks" || return 1
    status=0
    $ssh_n "$2" "mkdir -p longshore-bench/$id/repo" || return 1
    scp -q -o BatchMode=yes "$staged"/* "$2:longshore-bench/$id/" </dev/null || status=$?
    if [ "$status" -eq 0 ]; then
        packed=$(mktemp) || return 1
        COPYFILE_DISABLE=1 tar -cf "$packed" --exclude=target --exclude=.git . || status=$?
        if [ "$status" -eq 0 ]; then
            $ssh_raw "$2" "tar -xf - -C longshore-bench/$id/repo" <"$packed" || status=$?
        fi
        rm -f "$packed"
    fi
    if [ "$status" -eq 0 ]; then
        $ssh_n "$2" "cd longshore-bench/$id && sh linux-checks.sh . repo" || status=$?
    fi
    $ssh_n "$2" "rm -rf longshore-bench/$id" || true
    [ "$status" -eq 0 ] || return 1
    echo "== $1 on $2: ok"
}

only_here=0
hosts=$(grep -v '^[[:space:]]*\(#\|$\)' scripts/test-hosts || true)
while [ $# -gt 0 ]; do
    case "$1" in
        # What the container is started with: this machine and no other.
        --here)
            only_here=1
            shift
            ;;
        -*)
            echo "ERROR: unknown option $1" >&2
            exit 2
            ;;
        *)
            hosts="$hosts
$1 ${2:-}"
            shift
            [ $# -eq 0 ] || shift
            ;;
    esac
done
[ "$only_here" -eq 0 ] || hosts=""
hosts=$(printf '%s\n' "$hosts" | grep -v '^[[:space:]]*$' || true)
# Refuse a machine without a destination before anything is built.
printf '%s\n' "$hosts" | while read -r target dest; do
    if [ -n "$target" ] && [ -z "$dest" ]; then
        echo "ERROR: '$target' names no SSH destination; io_uring was not exercised there." >&2
        exit 1
    fi
done

if [ "$(uname -s)" = Linux ]; then
    run_here
else
    run_in_container
fi

failed=0
if [ -n "$hosts" ]; then
    # Read the host list on descriptor 3 so nothing in the loop can consume it.
    while read -r target dest <&3; do
        if ! run_on_host "$target" "$dest"; then
            echo "== $target on $dest: FAILED" >&2
            failed=$((failed + 1))
        fi
    done 3<<HOSTS
$hosts
HOSTS
fi
if [ "$failed" -ne 0 ]; then
    echo "real io_uring checks: $failed machine(s) failed" >&2
    exit 1
fi
echo "real io_uring checks: ok"
