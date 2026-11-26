# io_uring zero-copy receive on the DGX Spark pair: the spike

Group 1 of `add-tcp-zero-copy-receive`: does the card take it, and what
does it save. Raw runs: `m31-tcp-zero-copy-receive-spike-dgx-spark.jsonl`.
The programs are throwaway C, kept in `bench/spikes/zcrx/`: `zcprobe.c`
registers a queue and says whether it was taken, `zcrecv.c` receives
streams and counts.

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

One receiving thread, the queue's interrupt on core 19. "Receiver's own
CPU" is the thread's; the kernel's packet handling runs on the queue's
core and is in the last column.

| Receive | Receiver on | GB/s | Receiver's own CPU-seconds a GB | Queue's core in packet handling |
|---|---|---|---|---|
| Zero-copy, bytes not read | another core | 10.9 | 0.036 | 53 % |
| Zero-copy, every byte read | another core | 7.7 | 0.120 | 25 % |
| Kernel copy (`tcp_uring`, direct) | another core | 7.3 | 0.125 | 19 % |
| Zero-copy, bytes not read | the queue's core | 11.5 | 0.058 | (same core) |
| Zero-copy, every byte read | the queue's core | 6.2 | 0.160 | (same core) |
| Kernel copy (`tcp_uring`, direct) | the queue's core | 5.8 | 0.172 | (same core) |

- **Without the copy a receiving thread spends 0.036 CPU-seconds a GB
  against 0.125**, and one thread carries 10.9 to 11.5 GB/s (87 to
  92 Gb/s), close to what one interface of the port carries at all. The
  thread was not full (about 40 % of its core); what held it there was not
  looked for.
- **A consumer that reads every byte gains nothing to speak of**: 0.120
  against 0.125. Reading the payload for the first time costs what the
  copy cost; the copy only moved it into the cache. The saving is for
  payload that is passed on or written without being read.
- **Chunks are a page**: completions carried 3,530 bytes on average and
  never more than 4,096, three million completions a second at
  10.9 GB/s. Larger chunks (`rx_buf_len`) were not tried: they need huge
  pages set aside on the host, and none are.

## Go

The mode works on this card and removes two thirds of the receiving
thread's cost when payload is not read on the way. Groups 2 to 5 proceed.
Two things the design takes from this:

- The frame held for a log write still pays one move, and the frame whose
  bytes the caller reads pays the read: the gain is in forwarding,
  replication and anything that hands payload on. Group 5 measures those
  cases apart.
- A completion a page is a cost of its own; the mode must take many a
  turn, and larger chunks are worth a measurement once a host has huge
  pages.
