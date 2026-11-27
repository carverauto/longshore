#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
# Group 5's matrix: donbot receives on its second interface, roberto sends.
# run.sh OUT.jsonl [MTUS] [FRAMES] [MODES] [NS] [RUNS] [EXTRA_CELL_ARGS] [KIND] [FOP]
set -u
OUT=$1 MTUS=${2:-"9000 8244"} FRAMES=${3:-"324 65536 1048576"} MODES=${4:-"ring direct zc"}
NS=${5:-"1 3"} RUNS=${6:-3} SPARE=${7:-16384} KIND=${8:-bulk} FOP=${9:-on}
CONNS=2 LOAD_SECS=8 I=enP2p1s0f0np0
X() { ssh -o "ControlPath=/tmp/lsh-%C" -o BatchMode=yes mhansen@$1 "$2"; }
D=192.168.10.31 R=192.168.10.96
for mtu in $MTUS; do
  for h in $D $R; do X $h "sudo -n ip link set $I mtu $mtu"; done
  sleep 3
  for frame in $FRAMES; do for mode in $MODES; do for n in $NS; do for run in $(seq 1 $RUNS); do
    X $D "bash ~/zc/cell.sh $mode $n 40 $CONNS $SPARE $KIND $frame" > /tmp/g5cell.$$ &
    CELL=$!
    sleep 3
    X $R "for k in \$(seq 0 $((n-1))); do taskset -c \$((5+k)) ~/longshore-bench-src/target/release/tcp_uring $KIND --connect 10.100.33.1:\$((9311+k)) --conns $CONNS --frame $frame --frame-on-packet $FOP --secs $LOAD_SECS > /tmp/g5l\$k.json 2>&1 & done; wait"
    wait $CELL
    L=$(cat /tmp/g5cell.$$)
    LOADS=$(X $R "for k in \$(seq 0 $((n-1))); do grep '^{' /tmp/g5l\$k.json | tail -1; done" | paste -sd, -)
    [ -z "$L" ] && L='{"error":"no line"}'
    echo "{\"date\":\"$(date -u +%F)\",\"mtu\":$mtu,\"frame\":$frame,\"mode\":\"$mode\",\"cores\":$n,\"conns_a_core\":$CONNS,\"run\":$run,\"spare\":$SPARE,\"kind\":\"$KIND\",\"frame_on_packet\":\"$FOP\",\"cell\":$L,\"loads\":[$LOADS]}" >> $OUT
    echo "mtu=$mtu frame=$frame mode=$mode n=$n run=$run done" >&2
    sleep 2
  done; done; done; done
done
for h in $D $R; do X $h "sudo -n ip link set $I mtu 9000"; done
echo "matrix finished" >&2
