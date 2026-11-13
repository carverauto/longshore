## 1. Measuring
- [ ] 1.1 `Op::Connect` result carries the segment size; setting on the backend's limits
- [ ] 1.2 Backend: descriptor route, read `TCP_MAXSEG`, release the descriptor on every path
- [ ] 1.3 Simulator: a segment size per listening address
- [ ] 1.4 Real-kernel self-check: a loopback connect reports loopback's segment size, and a failed connect leaks no descriptor

## 2. Choosing
- [ ] 2.1 HTTP client reports address and size of each opened connection
- [ ] 2.2 Resolver: size per address, the choice in lookups, the minimum number of addresses, sizes across a refresh
- [ ] 2.3 Simulation tests: mixed sizes converge on the large-segment addresses; all alike changes nothing; setting off changes nothing

## 3. Real stores
- [ ] 3.1 Run on Linux 6.1 and on a current kernel
- [ ] 3.2 Five runs per cell against Amazon S3 from both instance types, with the setting on and off
- [ ] 3.3 Sweep the TLS send batch size on both instance types and set the default from it
- [ ] 3.4 README: measurement tables, the setting, and its trade-off
