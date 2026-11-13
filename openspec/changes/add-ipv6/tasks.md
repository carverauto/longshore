## 1. Addresses
- [x] 1.1 Address type covering both families in `io`, the backend and the simulator
- [x] 1.2 Real-kernel self-check over IPv6 loopback

## 2. Resolution
- [x] 2.1 AAAA encoding and parsing; property tests and fuzz target update (the repository keeps no fuzz corpus)
- [x] 2.2 Family preference and fallback to the other family when one fails to connect
- [x] 2.3 Simulation tests: IPv6 only, dual stack with one family unreachable

## 3. Real stores
- [x] 3.1 Run against a dual-stack store and record the result (`bench/results/m19-ipv6-linode.md`: Linode Object Storage)
- [ ] 3.3 Run against Amazon S3's dual-stack endpoint from an instance with an IPv6 address
- [x] 3.2 README: remove the IPv4-only limit
