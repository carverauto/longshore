# longshore_file

File I/O over the `Io` trait of `longshore_common`, and nothing built on it.

- `Files` is a fixed set of files and operations driven from the caller's
  own loop. It borrows the I/O backend on each call and never waits, so
  sockets (`longshore_tcp`), the S3 client and files share one ring and
  one thread.
- One API on io_uring, the portable `kqueue`/`epoll` backend and the
  deterministic simulator, with its disk faults and power loss.
- Handles that cannot reach a reused slot, operations reported once,
  whole transfers, ordered sequences, buffers lent without copies, direct
  I/O checked the same way on every backend, and one error type.
- No file can take every slot: each open file has an operation of its own.

Logs, pages, records and checksums are left to the program built on it.

## Benchmarks

The set against the raw `Op::File*` operations it is built on, on the
same backend, file and run, with `longshore-bench`'s `file_bench`
(`bench/README.md`). Three runs a cell; the order of the two paths
alternates. The full tables, setups and caveats are in the two results.

### DGX Spark NVMe, Linux, direct I/O (milestone 37)

[`bench/results/m37-file-bench-dgx-spark-nvme.md`](../../bench/results/m37-file-bench-dgx-spark-nvme.md):
the ring (`UringIo`) and the portable backend (`epoll`), 64 KiB and 1 MiB
transfers, depths 1 to 32, one sync at the end, a sync every 16 writes,
and durable writes; one 1 GiB file.

| | Result |
|---|---|
| The set against raw, GB/s | apart in 4 of 50 cells, 3 of them in the set's favour |
| The set against raw, CPU a GB | apart in 5 of 50 cells, 3 with the set lower |
| Reads, 1 MiB, from depth 16 | 13.4 GB/s on either path |
| Writes, 64 KiB, depth 32, the ring | 9.21 to 9.74 GB/s through the set, 7.86 to 8.92 raw (put down to the setup, not the crate) |
| Writes, 1 MiB, from depth 4 | 1.8 to 2.6 GB/s on both paths |
| CPU on the ring, from depth 4 | 0.008 to 0.149 CPU-s a GB (up to 0.205 with a sync every 16 writes) |
| A sync every 16 writes, 64 KiB, the ring | 0.74 of the write rate at depth 1, 0.32 at depth 32 |
| Durable writes, 64 KiB, depth 16 | 0.28 GB/s on the ring, 0.02 on the portable backend |

### Apple M3 Max, macOS, buffered and direct (milestone 36)

[`bench/results/m36-file-bench-macos.md`](../../bench/results/m36-file-bench-macos.md):
the portable backend (`kqueue`), 4 KiB, 64 KiB and 1 MiB transfers,
depths 1, 4 and 16, buffered and `F_NOCACHE`, three ways to sync,
sequential and random offsets: 108 cells on a 256 MiB file. The machine
was not idle, which shows as stalls of single operations.

| Passes | Set over raw, GB/s (median of 36 cells) | Set over raw, CPU a GB |
|---|---|---|
| Write, 4 KiB | 1.00 | 1.00 |
| Write, 64 KiB | 0.99 | 1.00 |
| Write, 1 MiB | 1.00 | 1.01 |
| Read, 4 KiB | 0.99 | 1.02 |
| Read, 64 KiB | 0.99 | 1.01 |
| Read, 1 MiB | 0.99 | 1.01 |

Reads cost the set about 1 to 2 % more of the thread's CPU a GB, on the
same side in most cells and in every run; writes show no difference.
Single cells say little: the runs of one cell differ by a median of 5 %.

Not measured: the crate's sequences and gathered writes, more than one
file, and a disk being filled. Both builds predate the fixes of the
reviews, which touch short transfers and error paths that no pass met.
