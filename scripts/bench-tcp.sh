#!/bin/sh
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Runs the measurements of the TCP connection set (`tcp_uring rr`, `bulk`
# and `path`) over loopback: the ring against the epoll baseline, and one
# setting at a time around a base configuration. One JSON line a run on
# standard output; summarise with scripts/bench-summary.py.
#
#   scripts/bench-tcp.sh [--secs N] [--runs N] > bench/results/NAME.jsonl
#
#   --secs N   how long each run measures (default 3)
#   --runs N   runs of each configuration (default 3)
#
# - On Linux: on this machine.
# - Elsewhere: in a Linux container, with `docker` or `podman`.
#
# Loopback in a container says which setting is better than which on that
# machine. It does not say what a network card will carry: those runs are
# made on the hosts that have one, with the same tool.
#
# Everything is a parameter; the script reads nothing from the environment.
set -eu

IMAGE="rust:1-bookworm"
secs=3
runs=3
here=0
while [ $# -gt 0 ]; do
    case "$1" in
        --secs | --runs)
            [ $# -ge 2 ] || { echo "bench-tcp: $1 needs a value" >&2; exit 2; }
            case "$2" in
                '' | *[!0-9]*) echo "bench-tcp: $1 takes a whole number" >&2; exit 2 ;;
            esac
            if [ "$1" = --secs ]; then secs=$2; else runs=$2; fi
            shift 2
            ;;
        --here) here=1; shift ;;
        *) echo "bench-tcp: unknown option $1" >&2; exit 2 ;;
    esac
done

if [ "$(uname -s)" != Linux ] && [ "$here" = 0 ]; then
    runtime=""
    for candidate in docker podman; do
        if command -v "$candidate" >/dev/null 2>&1; then
            runtime="$candidate"
            break
        fi
    done
    if [ -z "$runtime" ]; then
        echo "bench-tcp: this is not Linux and no container runtime (docker, podman) was found" >&2
        exit 1
    fi
    # io_uring is behind the default seccomp profile, and rings lock memory.
    exec "$runtime" run --rm \
        --security-opt seccomp=unconfined \
        --ulimit memlock=-1:-1 \
        --ulimit nofile=65536:65536 \
        -v "$PWD":/src:ro \
        -v longshore-cargo:/usr/local/cargo/registry \
        -v longshore-target:/target \
        -e CARGO_TARGET_DIR=/target \
        -w /src \
        "$IMAGE" sh scripts/bench-tcp.sh --here --secs "$secs" --runs "$runs"
fi

# 4,096 connections are twice as many sockets in one process, and rings
# lock their memory: both limits are raised where they can be, and said
# where they cannot.
ulimit -n 65536 2>/dev/null || echo "bench-tcp: cannot raise the limit of open files ($(ulimit -n)): runs of many connections will fail" >&2
ulimit -l unlimited 2>/dev/null || echo "bench-tcp: cannot raise the limit of locked memory ($(ulimit -l) KiB): runs of many connections may fail" >&2

cargo build --quiet --release -p longshore-bench --bin tcp_uring >&2
bin=$(cargo metadata --format-version 1 --no-deps | python3 -c '
import json, sys
print(json.load(sys.stdin)["target_directory"])
')/release/tcp_uring
dir=$(dirname "$bin")/bench-tcp-$$
mkdir -p "$dir"
trap 'rm -rf "$dir"' EXIT

# One configuration, `runs` times. A run that fails is said and counted.
failed=0
run() {
    i=0
    while [ "$i" -lt "$runs" ]; do
        if ! timeout 300 "$bin" "$@" --secs "$secs"; then
            echo "bench-tcp: failed: $*" >&2
            failed=$((failed + 1))
        fi
        i=$((i + 1))
    done
}

# Request and response at UltraGraphDB's frame sizes: the ring against
# epoll, by the number of connections.
for backend in uring epoll; do
    for conns in 1 16 64 256 1024 4096; do
        run rr --backend "$backend" --conns "$conns" --body 64
    done
    run rr --backend "$backend" --conns 16 --body 1024
done
# The receive mode and the size of a ring buffer.
for rx in ring direct; do
    for ring in 1024 4096 16384 65536; do
        run rr --rx "$rx" --ring "$ring" --conns 16 --body 64
        run rr --rx "$rx" --ring "$ring" --conns 16 --body 4096
    done
done
# Waiting for completions against spinning.
for wait in sleep spin; do
    run rr --wait "$wait" --conns 1 --body 64
    run rr --wait "$wait" --conns 16 --body 64
done

# Bulk, one way: the ring against epoll.
for backend in uring epoll; do
    run bulk --backend "$backend" --conns 4 --frame 65536
    run bulk --backend "$backend" --conns 4 --frame 1048576 --segment 1048576
done
# One setting at a time around: 4 connections, 64 KiB frames, ring mode,
# 16 KiB ring buffers, zero-copy from 16 KiB, 64 KiB segments.
for rx in ring direct; do
    run bulk --conns 4 --rx "$rx" --ring 16384
    run bulk --conns 4 --rx "$rx" --ring 16384 --frame 1048576 --segment 1048576
done
for ring in 4096 16384 65536; do
    run bulk --conns 4 --ring "$ring"
done
# 4294967295 is never: every send is copied by the kernel.
for zc in 4096 16384 65536 4294967295; do
    run bulk --conns 4 --ring 16384 --zc-min "$zc"
    run bulk --conns 4 --ring 16384 --zc-min "$zc" --frame 1048576 --segment 1048576
done
for segment in 65536 262144 1048576; do
    run bulk --conns 4 --ring 16384 --segment "$segment"
done

# The whole path: a block received, written to a preallocated file with
# direct I/O, made durable, sent on to three replicas and answered. A sync
# after each write against a file opened so that a write is durable when
# it completes; and waiting against spinning.
for backend in uring epoll; do
    for sync in fsync dsync; do
        run path --backend "$backend" --sync "$sync" --conns 1 --dir "$dir"
        run path --backend "$backend" --sync "$sync" --conns 16 --dir "$dir"
    done
done
for wait in sleep spin; do
    run path --wait "$wait" --sync dsync --conns 1 --dir "$dir"
done

if [ "$failed" -gt 0 ]; then
    echo "bench-tcp: $failed run(s) failed" >&2
    exit 1
fi
