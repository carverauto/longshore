# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

# Latency model for openspec/notes/s3-cache/s3-cache-draft.md, section 4. Assumptions, not measurements.
# Run: uv run --with numpy python openspec/notes/s3-cache/s3-cache-model.py
import numpy as np
rng=np.random.default_rng(7); N=2_000_000
Z=2.3263
def logn(med,p99,n): s=np.log(p99/med)/Z; return med*np.exp(s*rng.standard_normal(n))
def s3(med,p99,n):
    x=logn(med,p99,n)
    stall=rng.random(n)<0.002            # stalled/retried requests
    x[stall]+=rng.uniform(300,1500,stall.sum())
    return x
def hedged(med,p99,n):
    a=s3(med,p99,n); b=s3(med,p99,n)
    d=np.percentile(a,90)
    return np.minimum(a,d+b)+0.15        # late race at p90, plus the cache hop
def hits(n,fabric):
    if fabric=='aws':  mem=logn(0.15,0.5,n); nvme=logn(0.30,1.5,n)
    else:              mem=logn(0.04,0.10,n); nvme=logn(0.15,0.8,n)
    return np.where(rng.random(n)<0.7,mem,nvme)
def q(x): return [np.percentile(x,p) for p in (50,90,99,99.9)]
fmt=lambda v: " | ".join(f"{t:.2f}" if t<10 else f"{t:.0f}" for t in v)
for label,(med,p99) in {"small GET (<=64 KiB)":(25,100),"1 MiB GET":(50,200)}.items():
    print(f"\n### {label}; ms: p50 | p90 | p99 | p99.9")
    print("S3 direct |", fmt(q(s3(med,p99,N))))
    print("miss via cache, hedged |", fmt(q(hedged(med,p99,N))))
    for fab in ('aws','fabric'):
        xfer = 0 if med==25 else (1.7 if fab=='aws' else 0.08)  # 1 MiB at 5 Gbit/s flow vs ~100 Gbit/s
        print(f"hit only ({fab}) |", fmt(q(hits(N,fab)+xfer)))
        for h in (0.5,0.8,0.9,0.95,0.99,0.999):
            m=rng.random(N)<h
            x=np.where(m,hits(N,fab)+xfer,hedged(med,p99,N))
            print(f"h={h} ({fab}) |", fmt(q(x)))
