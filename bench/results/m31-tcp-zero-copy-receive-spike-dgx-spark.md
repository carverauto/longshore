# io_uring zero-copy receive on the DGX Spark pair: the spike

Group 1 of `add-tcp-zero-copy-receive`: does the card take it, and what
does it save. Raw runs: `m31-tcp-zero-copy-receive-spike-dgx-spark.jsonl`.
The programs are throwaway C, kept in `bench/spikes/zcrx/`: `zcprobe.c`
registers a queue and says whether it was taken, `zcrecv.c` receives
streams and counts, with zero-copy receive or with the kernel's copy.

**Hosts:** as milestone 30 (kernel 7.0.0-1019-nvidia, ConnectX-7
`mlx5_core`, firmware 28.45.4028), the second interface of the port,
MTU 9000. The sender is `tcp_uring bulk --conns 4 --frame 1048576
--segment 1048576` on the other machine, 5 seconds, one run each.

## Taken, with these settings

The registration (`IORING_REGISTER_ZCRX_IFQ`) is refused with `EINVAL`
while header and data split is off, and taken once it is on. What the
receiving machine needed, as root:

```
ethtool -G IFACE tcp-data-split on        # ethtool shows "n/a" before, "on" after
ethtool -K IFACE rx-gro-hw on
ethtool -K IFACE ntuple on
ethtool -X IFACE equal 19                 # queues 0..18 for everything else
ethtool -N IFACE flow-type tcp4 dst-ip ADDR dst-port PORT action 19
```

and the ring set up with single issuer, deferred task work and 32-byte
completions. The program ran as root; which capability is the least that
suffices was not tried.

## What it saves

One receiving thread on core 18, the queue's interrupt on core 19, four
connections. The same program (`zcrecv`, `gcc -O2`) receives on one ring
either with zero-copy receive or with the kernel's copy into a 1 MiB
buffer a connection, and either reads every byte or only counts. Two runs
a cell, both shown (`set` `second` in the raw file). "Thread" is the
receiving thread's own CPU; "queue's core" is the kernel's packet handling
on core 19 (its system and interrupt time over the bytes carried).

| Receive | Bytes | GB/s | Thread, CPU-s a GB | Queue's core, CPU-s a GB | Both |
|---|---|---|---|---|---|
| Zero-copy | not read | 8.3, 9.2 | 0.035, 0.034 | 0.033, 0.037 | 0.068, 0.071 |
| Zero-copy | read | 7.4, 7.7 | 0.117, 0.116 | 0.037, 0.039 | 0.154, 0.155 |
| Kernel copy | not read | 5.9, 6.1 | 0.129, 0.127 | 0.013, 0.015 | 0.142, 0.142 |
| Kernel copy | read | 5.3, 5.9 | 0.161, 0.157 | 0.010, 0.026 | 0.171, 0.183 |

- **Payload that is not read: half the CPU, counting both cores**
  (0.07 against 0.14), and about a quarter of it on the receiving thread
  alone (0.035 against 0.128). Part of what the thread saves moves to the
  queue's core, whose cost a byte more than doubles.
- **Payload that is read: about a quarter less on the thread** (0.117
  against 0.159), about a tenth less counting both cores (0.155 against
  0.177). Reading costs more after zero-copy (0.08 a GB) than after the
  copy (0.03), because the copy had brought the bytes into the cache.
- **Chunks are a page**: completions carried 3,535 bytes on average and
  never more than 4,096, some 2.5 million completions a second at 9 GB/s.
  Larger chunks (`rx_buf_len` of 8 KiB to 2 MiB, on huge pages) are
  refused by this driver with "operation not supported".
- In the first set of runs (`set` `first`, the table's first, third and
  fourth rows only, with `tcp_uring` as the copying receiver) the unread
  zero-copy run carried 10.9 GB/s; these carried 8.3 and 9.2. The thread
  was not full in either. What holds it there was not looked for.

What the program cannot show: that payload really came without a copy. It
relies on the flow rule above; a flow on another queue is, as far as the
kernel's documentation says, delivered by copying.

## An empty area

`zcrecv ... zc stall`: after a second the program returns no chunk until
nothing has arrived for a second, then returns everything and goes on.
One run, eight seconds, the same sender.

| | |
|---|---|
| Completions held when nothing more came | 72,750, on an area of 65,536 chunks |
| Dropped at the card meanwhile | 365 `rx_out_of_buffer`, 64 `rx19_buff_alloc_err` |
| Segments the sender sent again | 377 |
| From returning every chunk to the next byte | 5.4 seconds |
| Bytes received against bytes sent | 14.461 GB of 14.473 GB (the 12 frames short are the sender's, queued when it closed) |
| Most entries the refill ring ever held, in the other runs | 1,219 of 32,768 |

So a chunk backs more than one completion, an empty area makes the card
drop packets and TCP repair it, and the repair takes seconds. The mode
must stop receiving before the area is empty; `design.md` says how.

## How payload lies in the pages, and writing it to a file as it lies

The database's format on the wire is its format in the log, so the
question is whether received pages can go to a direct-I/O write with no
move at all. `zcrecv` counts how pieces lie; `zcrecv ... log` writes to a
file a connection with direct I/O, 1 MiB a write (set `log` in the raw
file; four connections, 5 seconds, two runs each).

| Packet size | Pieces that are a whole page |
|---|---|
| MTU 9000 (payload 8,948 bytes a packet) | 75 %; the stream's blocks almost never lie in one page |
| MTU 8244 (payload 8,192 = two pages) | 100 % |
| MTU 4148 (payload 4,096 = one page) | 100 % |

- **With a packet's payload a whole number of pages, every piece is a
  whole page.** The card puts each packet's payload at the start of a
  page, so a full packet fills its pages exactly.
- **But the stream's blocks only lie in those pages while every packet
  is full.** In three of five runs one short packet early in a
  connection (after about 62 KiB) shifted everything behind it: the
  pages are still whole, but a 4 KiB block of the stream then straddles
  two of them. The test program gives up on a connection at that point,
  so in those runs it wrote nothing as it lay (the next table shows the
  write itself).
- **What a copy and a write cost, for comparison:** received with the
  kernel's copy into a 1 MiB buffer and written from it, the thread
  spends 0.151 CPU-seconds a GB (2.7 GB/s, the disk's pace, one write at
  a time). Received without a copy and not written, 0.038.

**The write is accepted, and what it saves.** `zcrecv ... zc logpages`
writes every whole page as it lies, 256 pages a vectored direct-I/O
write, whatever block of the stream each holds; `copy log` receives with
the kernel's copy into a 1 MiB buffer and writes that. MTU 8244, three
runs each, one synchronous write at a time, so the disk sets the pace
(set `pages` in the raw file). Neither reads the payload.

| Receive and write | GB/s | Thread, CPU-s a GB |
|---|---|---|
| Pages written as they lie | 3.82, 3.75, 3.79 | 0.066, 0.068, 0.065 |
| Copied, then written | 2.72, 2.72, 2.72 | 0.154, 0.154, 0.155 |

From the card to the disk without a copy the thread spends 0.066
CPU-seconds a GB against 0.154, and carries 3.8 against 2.7 GB/s on
this disk. A receiver that checks a checksum over every byte pays for
that read on top, in both rows; the difference with it was not measured
here (the earlier table has it for payload that is not written: 0.117
against 0.159).

What is still missing is keeping the stream in step with the pages: a
frame lies in whole pages when it starts a packet.

**The disk alone.** `dd` with direct I/O of zeros to new files on the same
disk (Samsung MZALC4T0HBL1, ext4, no I/O scheduler), 6 to 17 GB a run,
one run each unless said; a second or two each, so a burst, not what the
disk sustains:

| Writers at once | Write size | GB/s |
|---|---|---|
| 1 | 1 MiB | 3.45, 3.25 |
| 4 | 1 MiB | 10.0 |
| 8 | 1 MiB | 12.5 |
| 16 | 1 MiB | 13.0 |
| 4 | 16 MiB | 13.1 |
| 1 | 128 KiB | 0.57 |

Sustained, 1 MiB writes for 22 seconds, the rate read from the disk's own
counters every two seconds:

| Writers at once | GB/s, every 2 s | Written |
|---|---|---|
| 8 | 13.1 for nine samples (18 s), then 3.1 | 246 GB |
| 1, twenty seconds after that run | 2.1, 2.9, 2.6, 2.9, 2.1, 1.5, 1.4, 1.3, 1.3, 1.4 | 42 GB |

The disk holds 13 GB/s for about 236 GB and then takes about 3. The
single writer ran after the eight had filled the disk's fast cache, so
its figures are of a disk still recovering, not of a fresh one.

So the 3.8 GB/s of "pages written as they lie" is what one write at a
time gets from this disk (3.3 to 3.5 with `dd`), not what the disk can
take: with eight or more writes in flight it takes about 13 GB/s, which
is also about what one interface of the port carries. The test program
writes synchronously, one write after another; the limit in that table
is the program's, and a version that keeps several writes in flight has
not been run.

## The body of a frame at a block, with the kernel's copy

UltraGraphDB's log takes a bulk load's body at a block boundary, while in
its own frame the body starts 256 bytes into a block. `zcrecv ... copy`
with a 1 MiB frame, every byte read once as a checksum would (set `lead`
in the raw file, three runs each, MTU 9000):

| | GB/s | Thread, CPU-s a GB |
|---|---|---|
| `logread`: written as received | 2.39, 2.26, 2.28 | 0.193, 0.201, 0.199 |
| `logshift`: body moved to a block-aligned buffer, then written | 2.24, 2.21, 2.21 | 0.203, 0.208, 0.205 |
| `loglead`: received 3,840 bytes into the buffer, so the body lies at a block | 2.39, 2.38, 2.37 | 0.192, 0.194, 0.190 |

Receiving the frame so that its body already lies at a block saves the
move: about 0.012 CPU-seconds a GB, 6 % of the thread, and about 7 %
more carried. Small, because the bytes are in the cache when they are
moved; but it costs one offset.

The same with `longshore_tcp` itself, once it had the option
(`HeaderFraming::with_body_at_block`): `tcp_uring path --header 256
--sync dsync --conns 64`, the log node on one Spark and its sources on
the other, a 4 KiB body logged, sent to three replicas and answered
(set `path` in the raw file; three runs each, in turn):

| The body | Blocks a second | Node's CPU a block |
|---|---|---|
| `--body-at frame`: moved to a block, then written | 10,237, 11,816, 10,948 | 10.92, 11.16, 11.13 µs |
| `--body-at block`: received at a block | 11,108, 10,813, 11,295 | 10.62, 10.16, 10.62 µs |

About 5 % less CPU a block on the node (10.5 against 11.1 µs), in line
with the 6 % of the test program. The rate does not move: it is the
disk's, one durable write a block.

## Go

The mode works on this card. It halves the CPU of receiving payload that
is handed on unread, and takes a quarter off the receiving thread when
the payload is read. Groups 2 to 5 proceed. What the design takes from
this:

- The saving is largest for forwarding and replication, smaller for
  payload the caller reads, and a frame held for a log write still pays
  one move. Group 5 measures the three apart.
- The kernel's packet handling gets dearer a byte in this mode: count the
  queue's core, not the thread alone.
- A completion a page is a cost of its own, and this card grants nothing
  larger: the mode must take many completions a turn.
