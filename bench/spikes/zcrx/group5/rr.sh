#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
#
# Request and answer, both ends sleeping or both spinning: how much of a
# round trip is the wait. The receiving host answers on core 9 (queue 19,
# core 19), the sending host asks from core 5. Run from a third machine
# with shared logins to both (ControlPath below).
# rr.sh OUT.jsonl WAIT RUNS
#
# Set in the environment, none of them defaulted:
#   SSH_USER    the login on both hosts
#   RECV_HOST   the answering host's address for SSH
#   SEND_HOST   the asking host's address for SSH
#   DATA_IP     the answering host's address on the interface under test
#   BENCH_BIN   the path of tcp_uring on both hosts
# Milestone 35 ran it as small.sh: from a third machine against the two
# DGX Sparks, RECV_HOST and SEND_HOST their LAN addresses, DATA_IP the
# answering host's address on the second interface of its 200 GbE port,
# BENCH_BIN a release build of tcp_uring in each one's copy of the tree.
set -u
usage="usage: SSH_USER=.. RECV_HOST=.. SEND_HOST=.. DATA_IP=.. BENCH_BIN=.. rr.sh OUT.jsonl WAIT RUNS"
for v in SSH_USER RECV_HOST SEND_HOST DATA_IP BENCH_BIN; do
  [ -n "${!v:-}" ] || { echo "$v is not set; $usage" >&2; exit 2; }
done
[ $# -ge 3 ] || { echo "$usage" >&2; exit 2; }
OUT=$1 WAIT=$2 RUNS=$3
X() { ssh -o "ControlPath=/tmp/lsh-%C" -o BatchMode=yes "$SSH_USER@$1" "$2"; }
D=$RECV_HOST R=$SEND_HOST
B=$BENCH_BIN
for run in $(seq 1 $RUNS); do
  X $D "sudo -n taskset -c 9 $B rr --listen $DATA_IP:9311 --rx ring --conns 2 --wait $WAIT --secs 40 > /tmp/rr.json 2> /tmp/rr.err" &
  PEER=$!
  sleep 3
  L=$(X $R "taskset -c 5 $B rr --connect $DATA_IP:9311 --conns 2 --wait $WAIT --secs 8 2>&1 | grep '^{' | tail -1")
  wait $PEER
  P=$(X $D "grep '^{' /tmp/rr.json | tail -1")
  echo "{\"date\":\"$(date -u +%FT%TZ)\",\"wait\":\"$WAIT\",\"run\":$run,\"peer\":${P:-null},\"load\":${L:-null}}" >> $OUT
  echo "wait=$WAIT run=$run done" >&2
  sleep 2
done
