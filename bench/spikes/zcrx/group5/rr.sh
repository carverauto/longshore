#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
#
# Request and answer, both ends sleeping or both spinning: how much of a
# round trip is the wait. donbot answers on core 9 (queue 19, core 19),
# roberto asks from core 5. rr.sh OUT.jsonl WAIT RUNS
set -u
OUT=$1 WAIT=$2 RUNS=$3
X() { ssh -o "ControlPath=/tmp/lsh-%C" -o BatchMode=yes mhansen@$1 "$2"; }
D=192.168.10.31 R=192.168.10.96
B='~/longshore-bench-src/target/release/tcp_uring'
for run in $(seq 1 $RUNS); do
  X $D "sudo -n taskset -c 9 $B rr --listen 10.100.33.1:9311 --rx ring --conns 2 --wait $WAIT --secs 40 > /tmp/rr.json 2> /tmp/rr.err" &
  PEER=$!
  sleep 3
  L=$(X $R "taskset -c 5 $B rr --connect 10.100.33.1:9311 --conns 2 --wait $WAIT --secs 8 2>&1 | grep '^{' | tail -1")
  wait $PEER
  P=$(X $D "grep '^{' /tmp/rr.json | tail -1")
  echo "{\"date\":\"$(date -u +%FT%TZ)\",\"wait\":\"$WAIT\",\"run\":$run,\"peer\":${P:-null},\"load\":${L:-null}}" >> $OUT
  echo "wait=$WAIT run=$run done" >&2
  sleep 2
done
