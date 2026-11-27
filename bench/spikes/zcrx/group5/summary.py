# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

"""Group 5's tables from the raw lines: one row a cell, runs side by side."""
import json
import sys
from collections import defaultdict

MODE = {"ring": "Ring", "direct": "Direct", "zc": "ZeroCopy"}


def size(frame):
    return {324: "324 B", 65536: "64 KiB", 1048576: "1 MiB"}.get(frame, str(frame))


def runs(paths):
    cells = defaultdict(list)
    for path in paths:
        for line in open(path):
            r = json.loads(line)
            c = r["cell"]
            peers = c.get("peers", [])
            gb = sum(p.get("gb_per_s", 0) * p.get("secs", 0) for p in peers)
            rate = sum(p.get("gb_per_s", 0) for p in peers)
            own = sum(p.get("cpu", 0) for p in peers)
            frames = sum(p.get("frames", 0) for p in peers)
            kept = sum(p.get("kept", 0) for p in peers)
            before, after = c.get("drops_before", {}), c.get("drops_after", {})
            drops = after.get("rx_out_of_buffer", 0) - before.get("rx_out_of_buffer", 0)
            loads = r.get("loads", [])
            p99 = max((l.get("p99_us", 0) for l in loads), default=None)
            key = (r["kind"], r["mtu"], r["frame"], r["mode"], r["cores"], r["frame_on_packet"], r["spare"])
            cells[key].append({
                "rate": rate,
                "own": own / gb if gb else 0,
                "threads": c.get("thread_cores_cpu_s", 0) / gb if gb else 0,
                "queues": c.get("queue_cores_cpu_s", 0) / gb if gb else 0,
                "kept": 100 * kept / frames if frames else 0,
                "drops": drops,
                "p99": p99,
                "errors": [p["error"] for p in peers if "error" in p],
            })
    return cells


def table(cells, show_p99=False):
    head = "| Kind | MTU | Frame | Mode | Cores | Sender's option | Spare | GB/s | Thread CPU-s a GB | Threads' cores | Queues' cores | Kept | Drops |"
    if show_p99:
        head += " p99 µs |"
    print(head)
    print("|" + "---|" * (head.count("|") - 1))
    for key in sorted(cells, key=lambda k: (k[0], -k[1], k[2], list(MODE).index(k[3]), k[4], k[5], -k[6])):
        kind, mtu, frame, mode, cores, fop, spare = key
        rs = cells[key]
        f = lambda name, fmt: ", ".join(fmt.format(r[name]) for r in rs)
        kept = f"{min(r['kept'] for r in rs):.0f} to {max(r['kept'] for r in rs):.0f} %" if mode == "zc" else "-"
        row = (f"| {kind} | {mtu} | {size(frame)} | {MODE[mode]} | {cores} | {fop} | {spare} | {f('rate', '{:.2f}')} | "
               f"{f('own', '{:.3f}')} | {f('threads', '{:.3f}')} | {f('queues', '{:.3f}')} | {kept} | {f('drops', '{}')} |")
        if show_p99:
            row += " " + ", ".join("-" if r["p99"] is None else f"{r['p99']:.0f}" for r in rs) + " |"
        errors = [e for r in rs for e in r["errors"]]
        if errors:
            row += f" errors: {errors[:1]}"
        print(row)


if __name__ == "__main__":
    table(runs(sys.argv[1:]), show_p99=any("rr" in p for p in sys.argv[1:]))
