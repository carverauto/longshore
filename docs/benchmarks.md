# Measurements

Files named in backticks, such as `m20-preference-and-comparison.md`, are in [`bench/results/`](../bench/results/), each with the conditions it was measured under.

All of these use 8 MiB objects, and one client thread unless a row says
otherwise. GB means 10^9 bytes.
Each row names its file in `bench/results/`, which has the full conditions.

## Against Amazon S3

One client thread, from instances in the same region as the bucket, with
the preference for large-segment addresses on. Medians of five runs, and
of eight for HTTPS PUT (`m20-preference-and-comparison.md`).

| Request | Graviton4, 300 Gbit/s card | AMD Zen 5, 200 Gbit/s card | Limit |
|---|---|---|---|
| PUT, plain HTTP | 36.7 GB/s | 22.4 GB/s | Graviton4: the network card. AMD: 22.4 GB/s is 179 Gbit/s on a 200 Gbit/s card with the client thread at 0.80, so neither the card nor the thread is clearly the limit |
| PUT with a CRC-64 checksum on every body | 16.4 GB/s | 13.3 GB/s | The client's core |
| GET | 4.4 GB/s | 7.9 GB/s | The client's core |
| HTTPS PUT, 3 MiB send batch | 5.2 GB/s | 7.3 GB/s | The client's core |
| HTTPS GET | 2.7 GB/s | 5.1 GB/s | The client's core |

Plain PUT fills the 300 Gbit/s card with half of the client's thread idle,
and keeps about four of the machine's cores busy, most of it kernel network
work on other cores. Without the preference the same instances give a
median of 23.2 and 11.8 GB/s for plain PUT, and runs differ by 45% or more,
because S3's name resolves to some addresses whose connections get small
TCP segments (`m18-s3-aws-large-instances.md`).

The rows bound by the client's core grow with more clients. Separate
client processes side by side, each on its own core and sharing nothing:

| Request | Graviton4, 8 clients | AMD, 4 clients | AMD, 8 clients |
|---|---|---|---|
| HTTPS PUT | 34.5 to 36.2 GB/s | 24.0 GB/s | 16.7 GB/s, one run |
| GET | 26.1 to 26.5 GB/s | 22.0 to 22.1 GB/s | 24.8 to 26.1 GB/s |
| HTTPS GET | 17.6 to 17.9 GB/s | 14.7 to 14.8 GB/s | 21.6 to 22.1 GB/s |

Four clients fill the AMD instance's card over HTTPS, and eight were slower
there than four. These are processes started by hand, three runs per cell
unless marked.

`longshore::client` does the same inside one process, with one I/O thread
per core. Medians of three runs (`m22-client-and-soak.md`):

| Request | Graviton4, 1 I/O thread | 4 | 8 | AMD, 1 I/O thread | 2 | 4 |
|---|---|---|---|---|---|---|
| HTTPS PUT with a CRC-64 checksum | 4.4 GB/s | 17.1 | 31.1 | 6.2 GB/s | 13.1 | 23.9 |
| HTTPS PUT, checksums off | 5.3 | 20.9 | 37.6 | 7.5 | 15.5 | 25.0 |
| GET | 4.6 | 15.6 | 26.7 | 8.0 | 14.5 | 22.4 |
| HTTPS GET | 2.7 | 10.1 | 18.0 | 5.3 | 9.5 | 15.0 |

The client sends a checksum with every upload unless told not to. With the
same work asked of both, it is never more than 4% below the store driven
directly and is up to 7% above it. One I/O thread with checksums off stores
37.6 GB/s over plain HTTP on the Graviton4 instance and 25.0 on the AMD one.
In a 30 minute HTTPS upload on four I/O threads, with every connection to S3
closed every 40 seconds and all traffic to it dropped for 5 seconds after
every fourth of those, the client stored 3.4 million objects and returned no
error to its callers. In an eight hour run of HTTPS fetches and stores
with the same failures injected, it fetched 20.4 million objects, stored
2.9 million, and returned no error. Every fetch was checked against the
object's pattern at 64 sampled offsets, and none differed. Its memory was
the same at the end as at the start (`m23-client-eight-hours.md`).
Multipart uploads through the client were checked against S3 in the same
milestone.

Object size matters below about 4 MiB. On 128 connections one thread
stores 2.9 GB/s of 1 MiB objects and 13.9 GB/s of 64 MiB objects. The time
a request takes fits about 32 ms plus 9 ms per MiB, so the fixed 32 ms
dominates small objects (`m21-object-sizes.md`).

## Compared with other clients

s5cmd 2.3.0 and the AWS CLI 2.40.2 with its CRT transfer client, on the
Graviton4 instance against the same bucket, moving 8,000 objects of 8 MiB
held in RAM. Best run of each, with the cores busy across the machine.

| Request | s5cmd | AWS CLI with CRT | One client here | Eight clients here |
|---|---|---|---|---|
| PUT over HTTP | 22.1 GB/s on 72.0 cores | 6.5 on 5.3 | 36.9 on 3.9 | Not run: one fills the card |
| PUT over HTTPS | 14.3 on 54.0 | 6.6 on 7.5 | 5.3 on 1.3 | 36.2 on 13.5 |
| GET over HTTP | 11.0 on 42.4 | 4.0 on 5.6 | 4.4 on 1.2 | 26.5 on 10.9 |
| GET over HTTPS | 11.5 on 43.0 | 4.0 on 6.2 | 2.8 on 1.1 | 17.9 on 10.5 |

One client thread is the fastest of these for plain PUT and uses the fewest
cores per byte for every request. For HTTPS and for GET, s5cmd on 42 to 54
cores in its best runs moves more in total than one client thread does.
Four clients side by side pass it for HTTPS PUT and plain GET on both
machines and for HTTPS GET on the AMD machine. For HTTPS GET on the
Graviton4 machine four clients give 9.9 to 10.0 GB/s against s5cmd's 11.2
to 11.5, and eight clients give 17.6 to 17.9. Those tools read and write files, which costs them a copy
that our benchmark does not make, and their runs lasted 3.0 to 17.2 seconds.
The same file has the AMD figures and the conditions.

An earlier 32-core Graviton4 instance with a 100 Gbit/s card is in
`m14-s3-aws-us-east-1.md`. Plain PUT and PUT with checksums both filled
that card at 12.2 GB/s.

## Between two machines

The client on one DGX Spark (Arm Cortex-X925) sends to a second one over
200 Gbit/s Ethernet. The peer is a small program of ours that discards what
it receives, so these figures show what the client costs and say nothing
about any store. Medians of five runs (`m15-dgx-repeated-with-machine-cpu.md`).

| Request | Result | Limit | Cores busy on the client machine |
|---|---|---|---|
| PUT, plain HTTP | 24.5 GB/s | The link | 2.5 |
| PUT with CRC-64 checksums | 14.7 GB/s | The client's core | 1.5 |
| GET, plain HTTP | 6.4 GB/s | The client's core | 1.5 |
| HTTPS PUT | 5.8 GB/s | The client's core | 1.3 |
| HTTPS PUT, faster AES routine | 7.6 GB/s | The client's core | 1.3 |
| HTTPS GET, faster AES routine | 4.1 GB/s | The client's core | 1.5 |

The "faster AES routine" rows use a build made with
`scripts/build-fast-aes-aarch64.sh`. AWS-LC has an 8-way AES-GCM routine for
Arm that it only uses on processors it recognises, and the Cortex-X925 is
not one of them. The script fixes AWS-LC's processor capabilities when it is
compiled. A binary built that way crashes on a processor without those
instructions, so it is for hardware you know. x86-64 and Graviton do not
need it.

## AES-GCM by processor

HTTPS speed can follow the processor's AES speed: in a profile of an HTTPS
upload on the DGX Spark, two thirds of the client's core is inside the
cipher. Sealing TLS records, one core, in GiB/s (`m13-aes-gcm-by-cpu.md`):

| Processor | GiB/s |
|---|---|
| AMD EPYC 9R05, Zen 5 (AWS `m8azn`) | 25.6 |
| AMD EPYC 9R45, Zen 5 (AWS `c8a`) | 22.2 |
| Intel Xeon 6975P-C (AWS `c8i`) | 16.2 |
| Arm Cortex-X925, faster routine (DGX Spark) | 12.2 |
| AMD EPYC 9R14, Zen 4 (AWS `c7a`) | 10.4 |
| Arm Cortex-X925, default build | 7.8 |
| AWS Graviton4 (AWS `c8g`) | 6.1 |

On the AMD `m8azn` instance HTTPS upload to S3 is 7.3 GB/s with large
segments and a 3 MiB send batch, and 3.5 GB/s through every address S3
returns with a 1 MiB batch, both with the client's thread full
(`m20-preference-and-comparison.md`). The profiles in
`m18-s3-aws-large-instances.md` show where the core goes in each case:
about half in AES-GCM with large segments, and mostly in the kernel's
transmit path with mixed addresses. HTTPS upload has not been measured
end to end on the Intel processor.

## Compared with the talk

The talk's figures are from its slides. Its hardware differs from ours and
the slides do not describe the method in detail, so read this as a rough
placement.

| Measurement | Talk | Here |
|---|---|---|
| PUT, plain HTTP | 25.2 GB/s per core | 36.7 GB/s to S3, with a 300 Gbit/s card full |
| PUT with CRC-64 checksums | 4.3 GB/s, the speed of its checksum | 16.4 GB/s to S3 |
| HTTPS PUT | No end-to-end figure. 7.4 GB/s for the cipher alone | 5.2 to 7.3 GB/s to S3, 7.6 GB/s between our machines |
| GET | 4.7 GB/s | 4.4 to 7.9 GB/s from S3, 6.4 GB/s between our machines |
| CRC-64 alone | 4.3 GB/s | 88 GB/s, and about 30 GB/s inside a PUT (`m5-checksums.md`) |
| AES-GCM alone | 7.4 GB/s | 6.5 to 27 GB/s, depending on the processor |

The talk reports "per core". Between our own machines plain PUT fills the
core the client runs on, and about 1.3 cores of work at 16 connections and
1.6 at 32 run on other cores, most of it in the kernel. To S3 at
36.7 GB/s the client's thread is half idle and about 3.4 more cores are
busy elsewhere on the machine. The slides do not say whether the
talk counts that work, so we cannot tell which of our numbers matches
theirs.

Request rates for small operations are in `m9-s3-ops-linode.md` and
`m14-s3-aws-us-east-1.md`. In both, the store's latency sets the rate and
the client is mostly idle.
