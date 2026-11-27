#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
#
# Where a small frame's cost goes: one receiver on donbot's core 9 (its
# queue, 19, interrupts on core 19), as many senders on roberto as it
# takes to keep that core busy, and a profile of cores 9 and 19 while it
# is. Run from a machine with shared logins to both (ControlPath below).
# small.sh OUT.jsonl MODE FOP RUNS [PROFILE_DIR]
set -u
OUT=$1 MODE=$2 FOP=$3 RUNS=$4 PROF=${5:-}
SENDERS=3 CONNS=4 FRAME=324 SECS=8
X() { ssh -o "ControlPath=/tmp/lsh-%C" -o BatchMode=yes mhansen@$1 "$2"; }
D=192.168.10.31 R=192.168.10.96
B='~/longshore-bench-src/target/release/tcp_uring'
busy() { # user nice system irq softirq steal of cores 9 and 19, in jiffies
  X $D "awk '/^cpu9 |^cpu19 /{print \$1, \$2+\$3+\$4+\$7+\$8+\$9, \$2+\$3, \$4, \$7+\$8}' /proc/stat | tr '\n' ' '"
}
for run in $(seq 1 $RUNS); do
  X $D "sudo -n taskset -c 9 $B bulk --listen 10.100.33.1:9311 --rx $MODE --conns $((SENDERS*CONNS)) --secs 40 > /tmp/sm.json 2> /tmp/sm.err" &
  PEER=$!
  sleep 3
  X $R "for k in \$(seq 0 $((SENDERS-1))); do taskset -c \$((5+k)) $B bulk --connect 10.100.33.1:9311 --conns $CONNS --frame $FRAME --frame-on-packet $FOP --secs $SECS > /tmp/sml\$k.json 2>&1 & done; wait" &
  LOAD=$!
  sleep 2
  B0=$(busy); T0=$(date +%s.%N)
  if [ -n "$PROF" ] && [ $run -eq 1 ]; then
    X $D "sudo -n perf record -q -C 9,19 -F 4999 -o /tmp/sm.perf -- sleep 4 >/dev/null 2>&1; sudo -n perf report -i /tmp/sm.perf --sort cpu,dso,sym --stdio --no-children -q 2>/dev/null | head -400" > "$PROF/$MODE-$FOP.txt"
  else
    sleep 4
  fi
  B1=$(busy); T1=$(date +%s.%N)
  wait $LOAD; wait $PEER
  P=$(X $D "grep '^{' /tmp/sm.json | tail -1")
  L=$(X $R "for k in \$(seq 0 $((SENDERS-1))); do grep '^{' /tmp/sml\$k.json | tail -1; done" | paste -sd, -)
  echo "{\"date\":\"$(date -u +%FT%TZ)\",\"mode\":\"$MODE\",\"frame_on_packet\":\"$FOP\",\"run\":$run,\"frame\":$FRAME,\"senders\":$SENDERS,\"conns_a_sender\":$CONNS,\"window\":{\"secs\":$(echo "$T1 - $T0" | bc),\"before\":\"$B0\",\"after\":\"$B1\"},\"peer\":${P:-null},\"loads\":[$L]}" >> $OUT
  echo "mode=$MODE fop=$FOP run=$run done" >&2
  sleep 2
done
