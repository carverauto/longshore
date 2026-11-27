#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
#
# file_bench matrix: direct I/O on one disk, both backends. Run on the
# machine whose disk is measured.
#
# Set in the environment, none of them defaulted:
#   BENCH_BIN   the path of file_bench
#   WORK_DIR    a directory on the disk under test: it takes the 1 GiB
#               file (fb.dat, removed at the end) and the output, fbm.jsonl,
#               fbm.err and fbm.log
# Milestone 37 ran it on a DGX Spark: BENCH_BIN a release build of
# file_bench in its copy of the tree, WORK_DIR a directory in its home on
# the NVMe disk.
set -u
usage="usage: BENCH_BIN=.. WORK_DIR=.. fbm.sh"
for v in BENCH_BIN WORK_DIR; do
  [ -n "${!v:-}" ] || { echo "$v is not set; $usage" >&2; exit 2; }
done
OUT=$WORK_DIR/fbm.jsonl; : > $OUT
F=$WORK_DIR/fb.dat
for b in uring posix; do for ws in 64k 1m; do for d in 1 4 16 32; do for se in 0 16; do
  [ $b = posix ] && [ $d -gt 4 ] && continue
  timeout 600 $BENCH_BIN --backend $b --direct --api both --phase both --write-size $ws --depth $d --sync-every $se --secs 3 --runs 3 --path $F >> $OUT 2>> $WORK_DIR/fbm.err
  echo "$b $ws $d $se rc=$?" >> $WORK_DIR/fbm.log
done; done; done; done
for b in uring posix; do
  timeout 600 $BENCH_BIN --backend $b --direct --dsync --api both --phase write --write-size 64k --depth 16 --secs 3 --runs 3 --path $F >> $OUT 2>> $WORK_DIR/fbm.err
  echo "$b dsync rc=$?" >> $WORK_DIR/fbm.log
done
rm -f $F; echo finished >> $WORK_DIR/fbm.log
