## Why
A store's name can resolve to addresses whose front ends accept different
TCP segment sizes. Amazon S3 in us-east-1 answered with eight addresses,
some advertising a maximum segment size of 8,228 bytes and others 1,432. On
a network card that does not segment in hardware, a connection with small
segments costs the client's thread several times more per byte. The
resolver spreads connections evenly over every address, so throughput
depends on how the addresses happened to fall: plain uploads from a
300 Gbit/s instance ran at a median of 29 GB/s through the resolver and at
36.9 GB/s with every connection on a large-segment address
(`bench/results/m18-s3-aws-large-instances.md`).

## What Changes
- A connect reports the connection's maximum segment size when the I/O
  backend is asked to measure it
- The resolver records the segment size seen for each address and, when the
  setting is on, hands out the addresses with large segments
- The setting is off by default, because it concentrates connections on
  fewer of the store's addresses

## Impact
- Affected specs: `segment-size-preference` (new)
- Affected code: `io` (connect result, backend, simulator), `http::client`
  (reports the size with the address), `dns::resolver` (per-address size and
  the choice between addresses), `store` configuration, `s3_uring`
- Depends on: `add-ipv6`, which changes the resolver's address sets
- First principles: unchanged on the data path. Measuring costs system calls
  when a connection is opened and none while data moves. Storage per
  address is fixed.
