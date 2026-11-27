# Milestone 36: `longshore::file` against the raw file operations on a Mac

Task 4.1 of `add-file-crate`, measured 2026-11-27 with `file_bench` on
`PosixIo` (`kqueue`), one thread. Raw lines:
`m36-file-bench-macos.jsonl`, 1,296 of them, one a pass, in the order run.

## Setup

- Machine: `Mac15,8`, Apple M3 Max, 16 cores, 128 GiB. macOS 27.2.1
  (build 26B131, Darwin 27.2.0). Not pinned to a core: macOS has no
  pinning.
- Disk: the internal `APPLE SSD AP8192Z`, 8 TB, APFS on the Data volume,
  FileVault on. The file was `/private/tmp/m36-file-bench/f.dat`, removed
  after each cell.
- Build: release, commit `d238b33` (the implementation of groups 1 and 2).
  The commit after it, `dbb83f5`, changes how many tokens the set counts
  and how `file_bench` sizes its table of tickets; it does not change the
  I/O path, and the runs were not repeated on it.
- The machine was not idle: other work, not recorded, ran on it during
  the matrix. That shows as stalls of single operations, described below.

## How it was measured

Every cell is

```text
file_bench --backend posix --write-size W --depth D --file-size 256m --secs 1 --runs 3
           --path /private/tmp/m36-file-bench/f.dat [--direct] [--sync-every 16 | --dsync] [--random]
```

for W of 4 KiB, 64 KiB and 1 MiB, D of 1, 4 and 16, buffered and
`--direct` (`F_NOCACHE`), the three ways to sync, and sequential and
random offsets: 108 cells, 23 minutes. Each cell makes a 256 MiB
file, reserves it and writes it once unmeasured, then runs three times;
each run opens the file through each API in turn, the order swapped every
run, and makes a write pass and a read pass of one second each. A pass
ends at the first operation it would hand out after one second, and then
waits for what is in flight.

- `file`: through `longshore::file::Files` (open, `write_at`, `read_at`,
  `sync`, close), with one file of `depth + 1` operations.
- `raw`: through `Op::FileWrite`, `Op::FileRead` and `Op::FileSync`
  submitted directly, on the same backend and the same buffers.
- Sync at the end: one data sync at the end of a write pass, inside its
  time. `--sync-every 16`: a data sync each time 16 writes have completed
  since the last, at most one in flight, writes going on meanwhile, plus
  the one at the end. `--dsync`: the file opened with `O_DSYNC`, no sync.
  On macOS a data sync is `F_FULLFSYNC`, which flushes the drive's cache;
  `O_DSYNC` does not (`OpenFlags::with_dsync`).
- GB/s: bytes of the pass over its time, 10^9 bytes. ops/s: writes or
  reads a second, syncs not counted. CPU-s/GB: the thread's CPU time
  (`CLOCK_THREAD_CPUTIME_ID`, user and system) over the pass's GB. p99:
  latency of a write or read from the call to its completion, kept in a
  histogram of about 3 % resolution. Sync p50 and p99: the same, of the
  syncs.
- Every figure below is the median of three runs of one cell, of one API.

`PosixIo` on macOS runs file operations synchronously inside `tick`
(`openspec/project.md`): operations in flight are performed one after
another by the thread, so depth does not put several at the drive at
once. Depth still gains where the poller's turn is shared by more
operations.

## The set's cost against the raw path

The ratio of the set's median to the raw path's median, cell by cell,
over every cell of each kind (each kind has 36 cells: two kinds of I/O,
three depths, three ways to sync, two orders of offsets; the read passes
of all three ways to sync are counted, which differ in nothing but the
file's state):

| Passes | Ratio of | Cells | Median | Lowest | Highest |
|---|---|---|---|---|---|
| Write, 4 KiB | GB/s | 36 | 1.00 | 0.86 | 8.62 |
| Write, 4 KiB | CPU-s per GB | 36 | 1.00 | 0.87 | 1.36 |
| Write, 64 KiB | GB/s | 36 | 0.99 | 0.83 | 1.08 |
| Write, 64 KiB | CPU-s per GB | 36 | 1.00 | 0.60 | 1.17 |
| Write, 1 MiB | GB/s | 36 | 1.00 | 0.86 | 1.07 |
| Write, 1 MiB | CPU-s per GB | 36 | 1.01 | 0.79 | 1.51 |
| Read, 4 KiB | GB/s | 36 | 0.99 | 0.94 | 1.10 |
| Read, 4 KiB | CPU-s per GB | 36 | 1.02 | 0.92 | 1.14 |
| Read, 64 KiB | GB/s | 36 | 0.99 | 0.80 | 1.17 |
| Read, 64 KiB | CPU-s per GB | 36 | 1.01 | 0.88 | 1.33 |
| Read, 1 MiB | GB/s | 36 | 0.99 | 0.79 | 1.11 |
| Read, 1 MiB | CPU-s per GB | 36 | 1.01 | 0.73 | 1.41 |

The three runs of one cell and one API differ by a median of 5 % in
GB/s, highest over lowest, and by 15 % or more in a tenth of them. The
cells where the two APIs differ most go both ways, and no kind of cell
has the set behind in all its cells. What the runs show: on this backend
the set costs no throughput and no CPU a GB that three runs of one second
can tell from the raw path's. They cannot show a cost of a few percent:
the largest median gap, 2 % more CPU a GB for the set at 4 KiB reads, is
within the runs' spread. The one ratio of 8.62 is a cell where the raw
path stalled in all three runs (below).

## Stalls

In 39 of the 1,296 passes a write, a read or a sync took 0.1 s or more:
21 through the set and 18 raw, all but one of them write passes. 16 of
them, all buffered 4 KiB sequential writes with `--dsync` (9 raw, 7
through the set), had an operation of 0.5 to 2.3 s. The others had
operations or syncs of 0.1 to 0.36 s, in cells of every write size. A
few passes of both APIs moved under a tenth of their cell's median, for
one a pass of direct 4 KiB random writes at depth 16 through the set: 48
writes in 1.3 s. The thread's CPU in such passes was under 1 %: it was
waiting for the kernel. Each figure below is a median of three runs, so a cell with
one such pass is not moved by it. The buffered 4 KiB `--dsync` sequential
cells had stalls in 16 of their 18 passes, and their figures are not a
measurement of either API.

No pass failed, hung or ended early. The set's path ran every cell.

## Writes, synced at the end

| I/O | Write size | Depth | file GB/s | raw GB/s | file CPU-s/GB | raw CPU-s/GB |
|---|---|---|---|---|---|---|
| buffered | 4 KiB | 1 | 0.24 | 0.24 | 1.450 | 1.478 |
| buffered | 4 KiB | 4 | 0.60 | 0.60 | 1.009 | 1.016 |
| buffered | 4 KiB | 16 | 0.89 | 0.89 | 0.911 | 0.908 |
| buffered | 64 KiB | 1 | 3.13 | 3.12 | 0.159 | 0.159 |
| buffered | 64 KiB | 4 | 4.53 | 5.44 | 0.167 | 0.143 |
| buffered | 64 KiB | 16 | 6.61 | 6.73 | 0.130 | 0.130 |
| buffered | 1 MiB | 1 | 9.17 | 9.06 | 0.085 | 0.084 |
| buffered | 1 MiB | 4 | 13.48 | 13.43 | 0.068 | 0.068 |
| buffered | 1 MiB | 16 | 12.57 | 12.52 | 0.075 | 0.075 |
| direct | 4 KiB | 1 | 0.17 | 0.17 | 1.701 | 1.688 |
| direct | 4 KiB | 4 | 0.26 | 0.26 | 1.607 | 1.639 |
| direct | 4 KiB | 16 | 0.30 | 0.29 | 1.424 | 1.547 |
| direct | 64 KiB | 1 | 1.52 | 1.53 | 0.204 | 0.202 |
| direct | 64 KiB | 4 | 1.80 | 1.87 | 0.188 | 0.184 |
| direct | 64 KiB | 16 | 2.19 | 2.15 | 0.152 | 0.151 |
| direct | 1 MiB | 1 | 5.83 | 5.88 | 0.029 | 0.028 |
| direct | 1 MiB | 4 | 5.85 | 6.25 | 0.026 | 0.027 |
| direct | 1 MiB | 16 | 5.95 | 5.97 | 0.036 | 0.037 |

Random offsets, synced at the end:

| I/O | Write size | Depth | file GB/s | raw GB/s | file CPU-s/GB | raw CPU-s/GB |
|---|---|---|---|---|---|---|
| buffered | 4 KiB | 1 | 0.22 | 0.22 | 1.588 | 1.620 |
| buffered | 4 KiB | 4 | 0.53 | 0.52 | 1.125 | 1.170 |
| buffered | 4 KiB | 16 | 0.82 | 0.83 | 0.991 | 0.975 |
| buffered | 64 KiB | 1 | 3.00 | 2.98 | 0.149 | 0.150 |
| buffered | 64 KiB | 4 | 5.17 | 5.27 | 0.142 | 0.138 |
| buffered | 64 KiB | 16 | 6.13 | 6.22 | 0.140 | 0.139 |
| buffered | 1 MiB | 1 | 12.55 | 12.70 | 0.066 | 0.065 |
| buffered | 1 MiB | 4 | 14.28 | 14.67 | 0.064 | 0.062 |
| buffered | 1 MiB | 16 | 13.15 | 12.98 | 0.072 | 0.072 |
| direct | 4 KiB | 1 | 0.03 | 0.03 | 5.299 | 4.287 |
| direct | 4 KiB | 4 | 0.03 | 0.03 | 4.473 | 4.205 |
| direct | 4 KiB | 16 | 0.03 | 0.03 | 4.774 | 4.454 |
| direct | 64 KiB | 1 | 1.25 | 1.37 | 0.218 | 0.201 |
| direct | 64 KiB | 4 | 1.75 | 1.83 | 0.163 | 0.170 |
| direct | 64 KiB | 16 | 1.79 | 1.82 | 0.152 | 0.166 |
| direct | 1 MiB | 1 | 5.72 | 5.62 | 0.029 | 0.029 |
| direct | 1 MiB | 4 | 6.03 | 5.94 | 0.028 | 0.027 |
| direct | 1 MiB | 16 | 5.81 | 5.95 | 0.033 | 0.030 |

The direct random 4 KiB rows are the one group where the set's median
CPU a GB is above the raw path's at all three depths (6 to 24 %); their
GB/s are the same. Single runs of either API in those cells range from
4.0 to 5.6 CPU-s a GB (and one pass of the set, which stalled, at 20.8),
so the gap lies inside the runs' spread and is not shown to be the
set's.

## Reads

The read passes of the cells synced at the end; those of the other cells
are in the raw lines and differ from these in nothing measured.

| I/O | Read size | Depth | file GB/s | raw GB/s | file CPU-s/GB | raw CPU-s/GB |
|---|---|---|---|---|---|---|
| buffered | 4 KiB | 1 | 0.28 | 0.30 | 0.733 | 0.642 |
| buffered | 4 KiB | 4 | 1.01 | 1.01 | 0.317 | 0.311 |
| buffered | 4 KiB | 16 | 2.56 | 2.61 | 0.222 | 0.213 |
| buffered | 64 KiB | 1 | 3.67 | 3.71 | 0.104 | 0.102 |
| buffered | 64 KiB | 4 | 6.51 | 8.17 | 0.106 | 0.080 |
| buffered | 64 KiB | 16 | 11.03 | 10.88 | 0.079 | 0.080 |
| buffered | 1 MiB | 1 | 13.12 | 13.08 | 0.066 | 0.066 |
| buffered | 1 MiB | 4 | 14.68 | 14.63 | 0.066 | 0.066 |
| buffered | 1 MiB | 16 | 8.97 | 8.63 | 0.111 | 0.115 |
| direct | 4 KiB | 1 | 0.12 | 0.13 | 2.031 | 1.941 |
| direct | 4 KiB | 4 | 0.17 | 0.17 | 1.994 | 1.835 |
| direct | 4 KiB | 16 | 0.19 | 0.20 | 1.743 | 1.573 |
| direct | 64 KiB | 1 | 0.46 | 0.44 | 0.173 | 0.181 |
| direct | 64 KiB | 4 | 0.42 | 0.43 | 0.179 | 0.167 |
| direct | 64 KiB | 16 | 0.45 | 0.44 | 0.147 | 0.147 |
| direct | 1 MiB | 1 | 3.60 | 3.71 | 0.021 | 0.021 |
| direct | 1 MiB | 4 | 3.73 | 3.74 | 0.020 | 0.019 |
| direct | 1 MiB | 16 | 3.47 | 3.65 | 0.026 | 0.027 |

Buffered reads of a 256 MiB file just written come from the page cache.
The buffered 64 KiB, depth 4 cell is the widest gap in the table, 20 %
against the set in GB/s, here and in the same cell's sequential writes;
the cells beside it at depths 1 and 16 show none, and the random reads at
depth 4 differ by 4 %. The medians of the random reads of these cells
(in the raw lines) are within 5 % of each other in every row.

## Syncs

A data sync every 16 writes, sequential offsets. On this disk a sync
takes 5 to 13 ms and sets the rate: about 16 writes a sync.

| I/O | Write size | Depth | file ops/s | raw ops/s | file sync p50 µs | raw sync p50 µs | file sync p99 µs | raw sync p99 µs |
|---|---|---|---|---|---|---|---|---|
| buffered | 4 KiB | 1 | 2486 | 2536 | 5898 | 5898 | 7995 | 7995 |
| buffered | 4 KiB | 4 | 2570 | 2667 | 5898 | 5374 | 8389 | 7995 |
| buffered | 4 KiB | 16 | 2659 | 2691 | 5636 | 5505 | 8913 | 8258 |
| buffered | 64 KiB | 1 | 2371 | 2409 | 6029 | 6029 | 8913 | 8258 |
| buffered | 64 KiB | 4 | 2363 | 2373 | 6160 | 6160 | 10486 | 8651 |
| buffered | 64 KiB | 16 | 2367 | 2386 | 6422 | 6422 | 9175 | 8651 |
| buffered | 1 MiB | 1 | 1461 | 1424 | 8389 | 8651 | 12321 | 11534 |
| buffered | 1 MiB | 4 | 1251 | 1210 | 11272 | 11010 | 13894 | 13632 |
| buffered | 1 MiB | 16 | 1203 | 1227 | 13107 | 12845 | 18350 | 17302 |
| direct | 4 KiB | 1 | 2178 | 2158 | 6029 | 5898 | 8126 | 9962 |
| direct | 4 KiB | 4 | 2225 | 2187 | 6292 | 6292 | 8913 | 8913 |
| direct | 4 KiB | 16 | 2195 | 2220 | 7340 | 7078 | 9699 | 9699 |
| direct | 64 KiB | 1 | 2517 | 2495 | 5636 | 5636 | 7733 | 7733 |
| direct | 64 KiB | 4 | 2322 | 2152 | 6029 | 6292 | 8258 | 9175 |
| direct | 64 KiB | 16 | 2298 | 2301 | 6685 | 6554 | 9175 | 9699 |
| direct | 1 MiB | 1 | 1203 | 1234 | 9437 | 9175 | 13369 | 12059 |
| direct | 1 MiB | 4 | 1318 | 1321 | 9699 | 9175 | 12321 | 12845 |
| direct | 1 MiB | 16 | 1279 | 1283 | 12321 | 12059 | 15466 | 15204 |

Durable writes (`--dsync`), sequential offsets. Each write waits for the
drive but not for its cache to be flushed, so these rates are not those
of the cells above at the same durability.

| I/O | Write size | Depth | file ops/s | raw ops/s | file p99 µs | raw p99 µs | file CPU-s/GB | raw CPU-s/GB |
|---|---|---|---|---|---|---|---|---|
| buffered | 4 KiB | 1 | 27437 | 3161 | 46 | 49 | 3.006 | 3.033 |
| buffered | 4 KiB | 4 | 8437 | 9738 | 119 | 123 | 2.577 | 2.590 |
| buffered | 4 KiB | 16 | 7518 | 8785 | 418 | 418 | 2.430 | 2.422 |
| buffered | 64 KiB | 1 | 22366 | 22487 | 53 | 53 | 0.247 | 0.245 |
| buffered | 64 KiB | 4 | 28081 | 27459 | 225 | 246 | 0.233 | 0.234 |
| buffered | 64 KiB | 16 | 30164 | 29655 | 590 | 704 | 0.223 | 0.230 |
| buffered | 1 MiB | 1 | 4463 | 4451 | 246 | 250 | 0.085 | 0.085 |
| buffered | 1 MiB | 4 | 4596 | 4582 | 983 | 967 | 0.086 | 0.087 |
| buffered | 1 MiB | 16 | 4362 | 4089 | 3867 | 4194 | 0.099 | 0.113 |
| direct | 4 KiB | 1 | 8438 | 8126 | 1671 | 1671 | 4.620 | 4.804 |
| direct | 4 KiB | 4 | 9573 | 9546 | 1868 | 1900 | 4.595 | 4.718 |
| direct | 4 KiB | 16 | 8318 | 8729 | 4456 | 4325 | 4.684 | 4.710 |
| direct | 64 KiB | 1 | 23156 | 22877 | 54 | 61 | 0.200 | 0.214 |
| direct | 64 KiB | 4 | 29383 | 30496 | 238 | 176 | 0.179 | 0.177 |
| direct | 64 KiB | 16 | 32112 | 32602 | 606 | 623 | 0.176 | 0.181 |
| direct | 1 MiB | 1 | 5654 | 5769 | 213 | 201 | 0.030 | 0.029 |
| direct | 1 MiB | 4 | 5961 | 6037 | 819 | 786 | 0.027 | 0.026 |
| direct | 1 MiB | 16 | 6090 | 6040 | 3080 | 3211 | 0.029 | 0.030 |

The buffered 4 KiB rows are the cells with the long stalls: 16 of their
18 passes had an operation of 0.5 to 2.3 s, so their rates say how often
a stall fell in a pass, not what either API does. The random-offset cells
of the same rows, with no such stall, ran 28,000 to 43,700 writes a
second, medians of either API (in the raw lines).

## What this does not show

- Anything of `UringIo`, or of Linux: the DGX Spark's NVMe is the other
  half of task 4.1.
- A cost of the set below a few percent: that needs longer passes and
  more runs than three of one second.
- Gathered writes, sequences, or several files: the bench drives one file
  with single writes, reads and syncs.
- Whether `FileSizes`'s defaults fit: the bench sizes its set itself
  (`depth + 1` operations for its one file, one path slot).
