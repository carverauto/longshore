#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
# Group 5's matrix: the receiving host receives on the interface under
# test, the sending host sends, each cell run by cell.sh on the receiver.
# Run from a third machine with shared logins to both (ControlPath below).
# run.sh OUT.jsonl [MTUS] [FRAMES] [MODES] [NS] [RUNS] [EXTRA_CELL_ARGS] [KIND] [FOP]
#
# Set in the environment, none of them defaulted:
#   SSH_USER    the login on both hosts
#   RECV_HOST   the receiving host's address for SSH
#   SEND_HOST   the sending host's address for SSH
#   DATA_IP     the receiver's address on the interface under test
#   IFACE       that interface's name, the same on both hosts (its MTU is
#               set on both, and left at 9000 at the end)
#   BENCH_BIN   the path of tcp_uring on both hosts
#   WORK_DIR    the directory on the receiver that holds cell.sh
# Milestone 34 ran it from a third machine against the two DGX Sparks:
# RECV_HOST and SEND_HOST their LAN addresses, IFACE the second interface
# of their 200 GbE ports and DATA_IP the receiver's address there,
# BENCH_BIN a release build of tcp_uring in each one's copy of the tree,
# WORK_DIR a scratch directory in the receiver's home.
set -u
usage="usage: SSH_USER=.. RECV_HOST=.. SEND_HOST=.. DATA_IP=.. IFACE=.. BENCH_BIN=.. WORK_DIR=.. run.sh OUT.jsonl [MTUS] [FRAMES] [MODES] [NS] [RUNS] [EXTRA_CELL_ARGS] [KIND] [FOP]"
for v in SSH_USER RECV_HOST SEND_HOST DATA_IP IFACE BENCH_BIN WORK_DIR; do
  [ -n "${!v:-}" ] || { echo "$v is not set; $usage" >&2; exit 2; }
done
[ $# -ge 1 ] || { echo "$usage" >&2; exit 2; }
OUT=$1 MTUS=${2:-"9000 8244"} FRAMES=${3:-"324 65536 1048576"} MODES=${4:-"ring direct zc"}
NS=${5:-"1 3"} RUNS=${6:-3} SPARE=${7:-16384} KIND=${8:-bulk} FOP=${9:-on}
CONNS=2 LOAD_SECS=8 I=$IFACE
X() { ssh -o "ControlPath=/tmp/lsh-%C" -o BatchMode=yes "$SSH_USER@$1" "$2"; }
D=$RECV_HOST R=$SEND_HOST
CELL_ENV="IFACE=$IFACE DATA_IP=$DATA_IP BENCH_BIN=$BENCH_BIN"
for mtu in $MTUS; do
  for h in $D $R; do X $h "sudo -n ip link set $I mtu $mtu"; done
  sleep 3
  for frame in $FRAMES; do for mode in $MODES; do for n in $NS; do for run in $(seq 1 $RUNS); do
    X $D "$CELL_ENV bash $WORK_DIR/cell.sh $mode $n 40 $CONNS $SPARE $KIND $frame" > /tmp/g5cell.$$ &
    CELL=$!
    sleep 3
    X $R "for k in \$(seq 0 $((n-1))); do taskset -c \$((5+k)) $BENCH_BIN $KIND --connect $DATA_IP:\$((9311+k)) --conns $CONNS --frame $frame --frame-on-packet $FOP --secs $LOAD_SECS > /tmp/g5l\$k.json 2>&1 & done; wait"
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
