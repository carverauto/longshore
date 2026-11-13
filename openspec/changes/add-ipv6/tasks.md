## 1. Addresses
- [ ] 1.1 Address type covering both families in `io`, the backend and the simulator
- [ ] 1.2 Real-kernel self-check over IPv6 loopback

## 2. Resolution
- [ ] 2.1 AAAA encoding and parsing; property tests and fuzz corpus update
- [ ] 2.2 Family preference and fallback to the other family when one fails to connect
- [ ] 2.3 Simulation tests: IPv6 only, dual stack with one family unreachable

## 3. Real stores
- [ ] 3.1 Run against a dual-stack S3 endpoint and record the result
- [ ] 3.2 README: remove the IPv4-only limit
