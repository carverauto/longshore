# M19: the store over IPv6 against Linode Object Storage

Date: 2026-11-13. This is a check that IPv6 works against a real store. It
is not a throughput measurement.

## Conditions

- Client: one Arm64 host on Linux 6.14 with a routed IPv6 address, reaching
  the store over the internet.
- Store: Linode Object Storage, `us-ord`. The bucket's name is a CNAME
  chain that ends in twelve AAAA records. The resolver keeps the first eight.
- Program: `s3_uring selftest`, which stores an object, stores it again
  conditionally, fetches it whole and by range, and erases it, with heap
  allocation forbidden while it runs.

## Result

| `S3_FAMILY` | Plain HTTP | HTTPS |
|---|---|---|
| `6` (AAAA records only) | passed | passed |
| `prefer6` (both, IPv6 first) | passed | passed |
| `4` (A records only) | passed | passed |

Every operation finished on its first attempt. A five second `put` run with
`S3_FAMILY=6` and eight connections stored 29 objects of 8 MiB, which
is 49 MB/s over this connection.

The real-kernel checks in `scripts/test-linux.sh` also pass on Linux 6.14
(Arm64) and Linux 6.8 (x86-64): the echo over the IPv6 loopback, with and
without zero-copy sends, a refused IPv6 connect, and an AAAA lookup through
the host's nameserver.

## Not covered

- Amazon S3's dual-stack endpoints. The instances used for the S3
  measurements have no IPv6 address.
- A preferred family that accepts no connections against a real store. The
  simulator covers a refused connection. A family that drops packets
  silently ends at the request timeout, and is not covered by a test.
