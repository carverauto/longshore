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
