## Why
The client resolves A records and connects over IPv4 only. Some networks are
IPv6 only, and S3 offers dual-stack endpoints.

## What Changes
- **BREAKING**: the `Io` trait and configuration take a socket address that
  can be IPv4 or IPv6
- AAAA queries in the resolver, with a configured preference between families
- IPv6 sockets in the `io_uring` backend and the simulator

## Impact
- Affected specs: `ipv6-networking` (new)
- Affected code: `io` (address type, connect), `sim`, `dns` (AAAA, cache
  entries per family), `http::client` and `store` configuration
- First principles: unchanged. Address storage stays fixed-size.
