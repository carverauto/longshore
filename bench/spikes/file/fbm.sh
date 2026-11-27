#!/bin/bash
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0
#
# file_bench matrix on donbot: direct I/O on the NVMe, both backends.
cd ~/longshore-bench-src
OUT=~/zc/fbm.jsonl; : > $OUT
F=/home/mhansen/zc/fb.dat
for b in uring posix; do for ws in 64k 1m; do for d in 1 4 16 32; do for se in 0 16; do
  [ $b = posix ] && [ $d -gt 4 ] && continue
  timeout 600 ./target/release/file_bench --backend $b --direct --api both --phase both --write-size $ws --depth $d --sync-every $se --secs 3 --runs 3 --path $F >> $OUT 2>> ~/zc/fbm.err
  echo "$b $ws $d $se rc=$?" >> ~/zc/fbm.log
done; done; done; done
for b in uring posix; do
  timeout 600 ./target/release/file_bench --backend $b --direct --dsync --api both --phase write --write-size 64k --depth 16 --secs 3 --runs 3 --path $F >> $OUT 2>> ~/zc/fbm.err
  echo "$b dsync rc=$?" >> ~/zc/fbm.log
done
rm -f $F; echo finished >> ~/zc/fbm.log
