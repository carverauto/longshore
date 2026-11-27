#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
# One cell on the receiving host: N receivers, receiver k on port 9311+k,
# queue 15+k (interrupts on core 15+k), thread on core 5+k. Prints one JSON
# line with each receiver's own line, and the CPU of the threads' and
# queues' cores and the card's drop counters over the cell.
# cell.sh MODE N SECS CONNS [ZC_SPARE] [KIND] [FRAME]
#
# Set in the environment, none of them defaulted (run.sh passes them on):
#   IFACE       the interface under test
#   DATA_IP     this host's address on it
#   BENCH_BIN   the path of tcp_uring here
# Milestone 34 ran it on the receiving DGX Spark through run.sh: IFACE the
# second interface of its 200 GbE port, DATA_IP its address there,
# BENCH_BIN a release build of tcp_uring in its copy of the tree.
set -u
usage="usage: IFACE=.. DATA_IP=.. BENCH_BIN=.. cell.sh MODE N SECS CONNS [ZC_SPARE] [KIND] [FRAME]"
for v in IFACE DATA_IP BENCH_BIN; do
  [ -n "${!v:-}" ] || { echo "$v is not set; $usage" >&2; exit 2; }
done
[ $# -ge 4 ] || { echo "$usage" >&2; exit 2; }
MODE=$1 N=$2 SECS=$3 CONNS=$4 SPARE=${5:-16384} KIND=${6:-bulk} FRAME=${7:-65536}
I=$IFACE
B=$BENCH_BIN
busy() { # busy jiffies of the cores given, from /proc/stat: user, nice,
  # system, irq, softirq and steal; not idle, and not iowait, which counts a
  # thread asleep in io_uring_enter as busy
  awk -v list="$1" 'BEGIN{n=split(list,c," "); for(i=1;i<=n;i++) want["cpu"c[i]]=1}
    want[$1]{b+=$2+$3+$4+$7+$8+$9} END{print b+0}' /proc/stat
}
drops() { ethtool -S $I | awk '/ rx_out_of_buffer:| rx_discards_phy:| rx_buffer_passed_thres_phy:/{gsub(":","",$1); printf "\"%s\":%s,", $1, $2}'; }
THREADS="" QUEUES=""
for k in $(seq 0 $((N-1))); do THREADS="$THREADS $((5+k))"; QUEUES="$QUEUES $((15+k))"; done
D0=$(drops); T0=$(busy "$THREADS"); Q0=$(busy "$QUEUES"); S0=$(date +%s.%N)
for k in $(seq 0 $((N-1))); do
  ZC=""; [ "$MODE" = zc ] && ZC="--zc-queue $I:$((15+k)) --zc-spare $SPARE"
  sudo -n taskset -c $((5+k)) $B $KIND --listen $DATA_IP:$((9311+k)) --rx $MODE $ZC --conns $CONNS --frame $FRAME --secs $SECS \
    > /tmp/g5p$k.json 2> /tmp/g5p$k.err &
done
wait
S1=$(date +%s.%N); T1=$(busy "$THREADS"); Q1=$(busy "$QUEUES"); D1=$(drops)
HZ=$(getconf CLK_TCK)
printf '{"window_s":%.2f,"thread_cores_cpu_s":%.3f,"queue_cores_cpu_s":%.3f,"drops_before":{%s},"drops_after":{%s},"peers":[' \
  "$(echo "$S1 - $S0" | bc)" "$(echo "($T1 - $T0) / $HZ" | bc -l)" "$(echo "($Q1 - $Q0) / $HZ" | bc -l)" "${D0%,}" "${D1%,}"
for k in $(seq 0 $((N-1))); do
  L=$(grep '^{' /tmp/g5p$k.json | tail -1); [ -z "$L" ] && L="{\"error\":\"$(tail -1 /tmp/g5p$k.err | tr -d '"')\"}"
  [ $k -gt 0 ] && printf ','; printf '%s' "$L"
done
echo ']}'
