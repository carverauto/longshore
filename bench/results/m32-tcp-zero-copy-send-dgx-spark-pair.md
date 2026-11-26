# Zero-copy sends over a card, after the fix: two DGX Sparks over 200 GbE

Task 4.3 of `add-tcp-zero-copy-receive`. `tcp_uring bulk`, one sending
thread on one Spark and one receiving thread on the other, each pinned to
a Cortex-X925 core, one interface of the port, MTU 9000, 3 seconds a run,
five rounds of all 45 cells in turn. Raw runs:
`m32-tcp-zero-copy-send-dgx-spark-pair.jsonl` (222 of 225; three runs
gave nothing because the test's own login to a machine was refused).
Hosts as milestone 30.

**What was wrong.** A zero-copy send was reported done only when the
kernel had let go of its memory, which over a card is after the peer's
acknowledgement, and a connection waited for that before its next send:
one segment a round trip (milestone 30: 0.7 to 1.2 GB/s in 64 KiB
segments). Now a send is reported when the kernel has taken its bytes
(`Op::SendZc`), the next follows at once, and only the memory waits.

**The columns.** "Copied": no zero-copy at all (`--zc-min` above every
length). "0": the old way, a send waiting for the last one's memory.
"1", "4", "8": that many zero-copy sends a connection may have with their
memory still out (`Sizes::with_zero_copy_sends`, `--zc-sends`); a send
beyond that is copied.

## GB/s received, medians of five

| Frame | Connections | Copied | 0 | 1 | 4 | 8 |
|---|---|---|---|---|---|---|
| 64 KiB | 1 | 4.86 | 0.64 | 5.55 | 5.34 | 5.10 |
| 64 KiB | 4 | 5.98 | 0.86 | 6.11 | 6.08 | 4.51 |
| 64 KiB | 16 | 5.70 | 3.73 | 5.87 | 5.67 | 5.81 |
| 256 KiB | 1 | 5.85 | 1.63 | 5.46 | 5.49 | 4.62 |
| 256 KiB | 4 | 6.23 | 3.72 | 6.11 | 6.19 | 6.14 |
| 256 KiB | 16 | 5.96 | 6.01 | 6.21 | 5.92 | 6.09 |
| 1 MiB | 1 | 4.97 | 2.71 | 5.15 | 4.95 | 5.40 |
| 1 MiB | 4 | 5.47 | 6.45 | 6.48 | 6.67 | 6.60 |
| 1 MiB | 16 | 4.08 | 6.54 | 6.05 | 6.47 | 6.37 |

## The sending thread's share of its core, medians of five

| Frame | Connections | Copied | 0 | 1 | 4 | 8 |
|---|---|---|---|---|---|---|
| 64 KiB | 1 | 47 % | 5 % | 51 % | 45 % | 38 % |
| 64 KiB | 4 | 47 % | 6 % | 52 % | 45 % | 31 % |
| 64 KiB | 16 | 68 % | 25 % | 65 % | 40 % | 35 % |
| 256 KiB | 1 | 53 % | 6 % | 49 % | 42 % | 34 % |
| 256 KiB | 4 | 67 % | 13 % | 55 % | 39 % | 29 % |
| 256 KiB | 16 | 100 % | 21 % | 81 % | 40 % | 28 % |
| 1 MiB | 1 | 95 % | 8 % | 81 % | 51 % | 24 % |
| 1 MiB | 4 | 100 % | 16 % | 93 % | 46 % | 18 % |
| 1 MiB | 16 | 100 % | 16 % | 100 % | 42 % | 18 % |

## What it shows

- **The fault is gone.** 64 KiB frames: 0.64 to 5.6 GB/s on one
  connection, 0.86 to 6.1 on four. In nearly every cell the receiving
  thread is now what holds the rate, at 5 to 6.7 GB/s.
- **Zero-copy is not slower than copying**, by the medians, with one or
  with four sends out: the least is 0.93 of the copied rate (256 KiB, one
  connection), which the requirement allows down to 0.9.
- **One send out a connection copies most of what it sends**, and the
  sending thread pays: a whole core for 1 MiB frames on sixteen
  connections against 42 % with four out and 18 % with eight.
- **Four is the default taken.** It carries as much as any setting in
  every cell and about halves the sender's CPU against one. Eight costs
  the sender least but was slower in two cells in all five rounds
  (64 KiB on four connections, 256 KiB on one), which is not explained.

## Checked on the way

One run of the matrix and one of a soak ended with the sender's
connection reset. In both the receiving end had not started: the test's
login to that machine was refused, and a connect to nothing is a reset.
Seventy further runs of the cell it happened in (256 KiB, sixteen
connections, eight out), with both ends' exit codes kept, had no failure
but one more refused login. In about one run in seven the receiving end
reports one connection closed by a reset at the very end: the load closes
its connections with a frame part-sent, which is an end of stream inside
a frame.

Not checked: closing a connection with sends out to a peer that has
gone, where the memory stays out until the kernel gives up on it.
