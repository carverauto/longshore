#!/usr/bin/env python3
# Copyright 2026 Carver Automation Corporation.
#
# Licensed under the Apache License, Version 2.0 (the "License").
# SPDX-License-Identifier: Apache-2.0

"""Summarise repeated benchmark runs.

Reads the JSON lines that http_uring and s3_uring print, one per run, from
the files given (or standard input), groups runs of the same configuration,
and prints a Markdown table with the median, the lowest and the highest
value of each cell. One run tells you little; five and a median is the
least worth publishing.

    for i in 1 2 3 4 5; do http_uring put --addr A --conns 16; done > runs.jsonl
    scripts/bench-summary.py runs.jsonl
"""
import json
import statistics
import sys

# What makes two runs the same configuration, and what to report about them.
KEY = ["cmd", "tls", "crc", "links", "conns", "threads", "size", "batch", "arch", "build"]
# Runs recorded before a field existed group with runs that use its default.
KEY_DEFAULTS = {"threads": 1}
METRICS = [
    ("gb_per_s", "GB/s", "{:.2f}"),
    ("requests_per_s", "req/s", "{:.0f}"),
    ("thread_cpu", "thread", "{:.2f}"),
    ("gb_per_cpu_s", "GB per CPU-s", "{:.1f}"),
    ("cpu_us_per_request", "CPU µs/req", "{:.0f}"),
]


def main() -> int:
    sources = [open(p) for p in sys.argv[1:]] or [sys.stdin]
    groups: dict[tuple, list[dict]] = {}
    skipped = 0
    for src in sources:
        for line in src:
            start = line.find("{")
            if start < 0:
                continue
            try:
                run = json.loads(line[start:])
            except json.JSONDecodeError:
                skipped += 1
                continue
            groups.setdefault(tuple(run.get(k, KEY_DEFAULTS.get(k)) for k in KEY), []).append(run)
    if not groups:
        print("no benchmark results found", file=sys.stderr)
        return 1

    used_keys = [k for i, k in enumerate(KEY) if any(g[i] is not None for g in groups)]
    used_metrics = [m for m in METRICS if any(m[0] in r for runs in groups.values() for r in runs)]
    header = used_keys + ["runs"] + [f"{label} (median, low to high)" for _, label, _ in used_metrics]
    print("| " + " | ".join(header) + " |")
    print("|" + "---|" * len(header))
    for group, runs in groups.items():
        cells = [str(group[KEY.index(k)]) if group[KEY.index(k)] is not None else "-" for k in used_keys]
        cells.append(str(len(runs)))
        for name, _, fmt in used_metrics:
            values = [r[name] for r in runs if name in r]
            if not values:
                cells.append("-")
            elif len(values) == 1:
                cells.append(fmt.format(values[0]))
            else:
                cells.append(f"{fmt.format(statistics.median(values))} ({fmt.format(min(values))} to {fmt.format(max(values))})")
        print("| " + " | ".join(cells) + " |")
    if skipped:
        print(f"\n{skipped} line(s) could not be read as JSON and were skipped.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
